import { useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Card,
  Dropdown,
  Empty,
  Form,
  Input,
  Modal,
  Popconfirm,
  Select,
  Space,
  Switch,
  Table,
  Tabs,
  Tag,
  Tooltip,
  Typography,
  message,
} from 'antd';
import { ApiOutlined, AppstoreOutlined, CheckCircleOutlined, DeleteOutlined, EditOutlined, InfoCircleOutlined, PlusOutlined, QuestionCircleOutlined, RobotOutlined, SafetyCertificateOutlined, ThunderboltOutlined } from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { apiKeysApi, rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { ApiKeyRecord, RdAgentMarketItem, RdAgentProfile, RdAgentWorkflow, RdIntegration, RdRepository, RdSteeringRule } from '@/types';

const { Text, Title, Paragraph } = Typography;

type AgentFormValues = {
  name: string;
  rolePrompt: string;
  defaultModel?: string;
  allowedToolsText?: string;
  enabled?: boolean;
};

type SteeringFormValues = {
  repositoryIds?: string[];
  name: string;
  description?: string;
  contentMd: string;
  enabled?: boolean;
};

type IntegrationFormValues = {
  provider: string;
  name: string;
  configText?: string;
  enabled?: boolean;
};

type WorkflowFormValues = {
  name: string;
  description?: string;
  definitionText: string;
  enabled?: boolean;
};

type SteeringTemplate = {
  key: string;
  name: string;
  description: string;
  contentMd: string;
};

function isRdChatModel(key: ApiKeyRecord): boolean {
  if (!key.enabled || key.model_type !== 'chat') return false;
  if (key.runtime_available === false) return false;
  const scenarios = key.scenarios;
  return !scenarios || scenarios.length === 0 || scenarios.includes('rd') || scenarios.includes('agent');
}

function modelLabel(key: ApiKeyRecord): string {
  return `${key.model || key.name} · ${key.provider}`;
}

function parseAllowedTools(text?: string) {
  const value = text?.trim();
  if (!value) return null;
  try {
    const parsed = JSON.parse(value) as unknown;
    const validateTools = (tools: unknown) => {
      if (typeof tools === 'string') {
        if (!tools.trim()) throw new Error('allowedTools string cannot be empty');
        return;
      }
      if (!Array.isArray(tools) || tools.some((tool) => typeof tool !== 'string' || !tool.trim())) {
        throw new Error('allowedTools must contain only non-empty tool names');
      }
    };
    if (typeof parsed === 'string' || Array.isArray(parsed)) {
      validateTools(parsed);
      return parsed;
    }
    if (parsed && typeof parsed === 'object') {
      const object = parsed as Record<string, unknown>;
      const tools = object.tools ?? object.allowedTools ?? object.allowed_tools;
      if (tools === undefined) {
        throw new Error('allowedTools object must contain tools, allowedTools, or allowed_tools');
      }
      validateTools(tools);
      return object;
    }
    throw new Error('allowedTools must be a string, string array, or object');
  } catch {
    throw new Error('allowedTools must be valid JSON using one of the documented schemas');
  }
}

function prettyJson(value: unknown): string {
  if (value == null) return '';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return '';
  }
}

function parseConfigJson(text?: string) {
  const value = text?.trim();
  if (!value) return {};
  try {
    const parsed = JSON.parse(value) as unknown;
    if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') {
      throw new Error('config must be a JSON object');
    }
    return parsed as Record<string, unknown>;
  } catch {
    throw new Error('config must be valid JSON object');
  }
}

function parseWorkflowDefinition(text?: string) {
  const parsed = parseConfigJson(text);
  if (!Array.isArray(parsed.stages) || parsed.stages.length === 0) {
    throw new Error('definition.stages must be a non-empty array');
  }
  return parsed;
}

function defaultWorkflowDefinition() {
  return JSON.stringify({
    version: 1,
    stages: [
      {
        id: 'architecture',
        agent: 'Architecture Agent',
        mode: 'ask',
        goal: '理解仓库结构、相关文件、风险和验证命令',
      },
      {
        id: 'implementation',
        agent: 'Coding Agent',
        mode: 'modify',
        goal: '生成可审查 Diff，不直接写主仓库',
      },
      {
        id: 'review',
        agent: 'Review Agent',
        mode: 'review',
        goal: '输出 findings-first 审查和风险',
      },
    ],
  }, null, 2);
}


function workflowStages(definition?: Record<string, unknown> | null) {
  const stages = definition?.stages;
  if (!Array.isArray(stages)) return [];
  return stages
    .filter((stage): stage is Record<string, unknown> => Boolean(stage) && typeof stage === 'object' && !Array.isArray(stage))
    .map((stage, index) => ({
      id: String(stage.id ?? `stage-${index + 1}`),
      agent: String(stage.agent ?? ''),
      goal: String(stage.goal ?? ''),
      mode: String(stage.mode ?? ''),
    }));
}

const INTEGRATION_PROVIDERS = [
  { value: 'github', labelKey: 'rd.integrationProviders.github', fallback: 'GitHub' },
  { value: 'gitlab', labelKey: 'rd.integrationProviders.gitlab', fallback: 'GitLab' },
  { value: 'jira', labelKey: 'rd.integrationProviders.jira', fallback: 'Jira' },
  { value: 'sentry', labelKey: 'rd.integrationProviders.sentry', fallback: 'Sentry' },
  { value: 'custom', labelKey: 'rd.integrationProviders.custom', fallback: 'Custom' },
];

function integrationConfigPlaceholder(provider?: string) {
  switch (provider) {
    case 'github':
      return '{\n  "apiBase": "https://api.github.com",\n  "token": "ghp_xxx",\n  "repository": "owner/repo"\n}';
    case 'gitlab':
      return '{\n  "apiBase": "https://gitlab.com/api/v4",\n  "privateToken": "glpat_xxx",\n  "projectPath": "group/project"\n}';
    case 'jira':
      return '{\n  "baseUrl": "https://your-company.atlassian.net",\n  "email": "dev@example.com",\n  "apiToken": "xxx"\n}';
    case 'sentry':
      return '{\n  "apiBase": "https://sentry.io/api/0",\n  "authToken": "sntrys_xxx",\n  "organization": "your-org"\n}';
    default:
      return '{\n  "url": "https://example.com/aos-webhook",\n  "token": "optional"\n}';
  }
}

export default function RdAgents() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [agentModalOpen, setAgentModalOpen] = useState(false);
  const [steeringModalOpen, setSteeringModalOpen] = useState(false);
  const [integrationModalOpen, setIntegrationModalOpen] = useState(false);
  const [editingAgent, setEditingAgent] = useState<RdAgentProfile | null>(null);
  const [editingRule, setEditingRule] = useState<RdSteeringRule | null>(null);
  const [editingIntegration, setEditingIntegration] = useState<RdIntegration | null>(null);
  const [workflowModalOpen, setWorkflowModalOpen] = useState(false);
  const [editingWorkflow, setEditingWorkflow] = useState<RdAgentWorkflow | null>(null);
  const [marketSearchText, setMarketSearchText] = useState('');
  const [marketQuery, setMarketQuery] = useState('');
  const [marketType, setMarketType] = useState<string | undefined>(undefined);
  const [marketInstallFilter, setMarketInstallFilter] = useState<'all' | 'installed' | 'not_installed'>('all');
  const [helpModal, setHelpModal] = useState<'overview' | 'allowedTools' | 'integration' | null>(null);
  const [agentForm] = Form.useForm<AgentFormValues>();
  const [steeringForm] = Form.useForm<SteeringFormValues>();
  const [integrationForm] = Form.useForm<IntegrationFormValues>();
  const [workflowForm] = Form.useForm<WorkflowFormValues>();
  const selectedIntegrationProvider = Form.useWatch('provider', integrationForm);

  const profilesQuery = useQuery({
    queryKey: queryKeys.rd.agentProfiles(),
    queryFn: rdApi.listAgentProfiles,
  });
  const steeringQuery = useQuery({
    queryKey: queryKeys.rd.steeringRules(),
    queryFn: rdApi.listSteeringRules,
  });
  const integrationsQuery = useQuery({
    queryKey: queryKeys.rd.integrations(),
    queryFn: rdApi.listIntegrations,
  });
  const marketQueryResult = useQuery({
    queryKey: queryKeys.rd.agentMarket({ q: marketQuery, itemType: marketType }),
    queryFn: () => rdApi.searchAgentMarket({ q: marketQuery, itemType: marketType }),
  });
  const marketItems = useMemo(() => {
    const items = marketQueryResult.data?.items ?? [];
    if (marketInstallFilter === 'installed') return items.filter((item) => item.installed);
    if (marketInstallFilter === 'not_installed') return items.filter((item) => !item.installed);
    return items;
  }, [marketInstallFilter, marketQueryResult.data?.items]);
  const workflowsQuery = useQuery({
    queryKey: queryKeys.rd.agentWorkflows(),
    queryFn: rdApi.listAgentWorkflows,
  });
  const repositoriesQuery = useQuery({
    queryKey: queryKeys.rd.repositories(),
    queryFn: rdApi.listRepositories,
  });
  const apiKeysQuery = useQuery({
    queryKey: queryKeys.apiKeys.list(),
    queryFn: apiKeysApi.list,
  });

  const modelOptions = useMemo(() => {
    const seen = new Set<string>();
    return (apiKeysQuery.data?.keys ?? [])
      .filter(isRdChatModel)
      .filter((key) => {
        const value = key.model || key.name;
        if (seen.has(value)) return false;
        seen.add(value);
        return true;
      })
      .sort((a, b) => (a.priority ?? 100) - (b.priority ?? 100))
      .map((key) => ({ value: key.model || key.name, label: modelLabel(key) }));
  }, [apiKeysQuery.data?.keys]);

  const repositories = repositoriesQuery.data?.repositories ?? [];
  const repositoryOptions = repositories.map((repo) => ({
    value: repo.id,
    label: `${repo.name} · ${repo.branch}`,
  }));
  const repositoryById = useMemo(() => new Map(repositories.map((repo) => [repo.id, repo])), [repositories]);
  const steeringTemplates = useMemo<SteeringTemplate[]>(() => [
    {
      key: 'ai-code',
      name: t('rd.steeringTemplateAiName', 'AI 代码修改通用规范'),
      description: t('rd.steeringTemplateAiDesc', '适合全局启用，约束 Agent 先计划、最小改动、Diff-first、保留测试和风险说明。'),
      contentMd: t('rd.steeringTemplateAiContent', [
        '## AI 代码修改通用规范',
        '- 先理解需求、相关文件和风险，再生成修改计划。',
        '- 只修改与任务直接相关的代码，不做无关重构或格式化。',
        '- 所有变更必须以可审查 Diff 呈现，不能静默覆盖文件。',
        '- 优先复用项目现有架构、命名、错误处理、i18n 和测试风格。',
        '- 输出必须包含：变更摘要、验证方式、残留风险。',
      ].join('\n')),
    },
    {
      key: 'security',
      name: t('rd.steeringTemplateSecurityName', '安全红线（OWASP 风格）'),
      description: t('rd.steeringTemplateSecurityDesc', '适合所有仓库或后端仓库，强调输入校验、鉴权、敏感信息和安全错误处理。'),
      contentMd: t('rd.steeringTemplateSecurityContent', [
        '## 安全红线',
        '- 所有外部输入必须校验、规范化，并避免信任客户端传入的权限字段。',
        '- 数据库访问禁止字符串拼接 SQL；必须使用参数化查询或 ORM 绑定。',
        '- 不在日志、错误信息、前端响应中泄露 token、密钥、连接串或个人敏感信息。',
        '- 权限判断必须在服务端执行；涉及租户、用户、仓库、操作级权限时显式校验。',
        '- 文件、URL、命令执行相关逻辑必须防路径穿越、SSRF、命令注入和越权访问。',
      ].join('\n')),
    },
    {
      key: 'rust',
      name: t('rd.steeringTemplateRustName', 'Rust 工程规范'),
      description: t('rd.steeringTemplateRustDesc', '适合 Rust 服务端仓库，强调 Result 错误处理、异步边界、可测试性和格式化。'),
      contentMd: t('rd.steeringTemplateRustContent', [
        '## Rust 工程规范',
        '- 生产路径避免 `unwrap`/`expect`，优先返回结构化错误并保留上下文。',
        '- 异步函数不要持有阻塞锁跨 `.await`；耗时 IO/CPU 操作要明确边界。',
        '- SQL/JSON/外部 API 解码要处理缺字段、空值和类型不匹配。',
        '- 公共函数保持职责单一，复杂逻辑拆到可单测 helper。',
        '- 提交前优先考虑 `cargo fmt`、相关 crate 的 `cargo check` 和必要单测。',
      ].join('\n')),
    },
    {
      key: 'frontend',
      name: t('rd.steeringTemplateFrontendName', 'React/TypeScript 前端规范'),
      description: t('rd.steeringTemplateFrontendDesc', '适合 WebUI 仓库，强调类型安全、i18n、可访问性、响应式和清晰状态管理。'),
      contentMd: t('rd.steeringTemplateFrontendContent', [
        '## React/TypeScript 前端规范',
        '- 组件状态保持清晰，避免把服务端状态、表单状态和 UI 临时状态混在一起。',
        '- 用户可见文案必须接入 i18n，按钮、状态、空态、错误提示都要覆盖中英文。',
        '- 表格、弹窗、抽屉、上传、危险操作必须有明确加载态、错误态和确认提示。',
        '- 样式遵循当前设计系统，不引入突兀颜色；移动端和窄屏不能出现不可操作区域。',
        '- TypeScript 类型应从 API 契约出发，避免 `any` 和脆弱的字段猜测。',
      ].join('\n')),
    },
    {
      key: 'commits-pr',
      name: t('rd.steeringTemplateCommitName', '提交与 PR 规范'),
      description: t('rd.steeringTemplateCommitDesc', '适合团队协作仓库，约束提交信息、PR 描述、测试证据和风险披露。'),
      contentMd: t('rd.steeringTemplateCommitContent', [
        '## 提交与 PR 规范',
        '- 提交标题使用 `type(scope): summary`，例如 `fix(rd): handle empty repository scope`。',
        '- PR 描述必须包含：背景、主要改动、验证结果、兼容性影响、风险与回滚方式。',
        '- 修复类任务要说明根因；功能类任务要说明新能力和边界。',
        '- 如果未运行测试，必须明确说明原因和建议补测命令。',
        '- 不把临时调试日志、密钥、个人本地路径或无关格式化混入 PR。',
      ].join('\n')),
    },
  ], [t]);

  const saveAgentMutation = useMutation({
    mutationFn: (values: AgentFormValues) => {
      const payload = {
        name: values.name,
        rolePrompt: values.rolePrompt,
        defaultModel: values.defaultModel,
        allowedTools: parseAllowedTools(values.allowedToolsText),
        enabled: values.enabled ?? true,
      };
      return editingAgent
        ? rdApi.updateAgentProfile(editingAgent.id, payload)
        : rdApi.createAgentProfile(payload);
    },
    onSuccess: () => {
      message.success(t('rd.configSaved', '配置已保存'));
      setAgentModalOpen(false);
      setEditingAgent(null);
      agentForm.resetFields();
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentProfiles() });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const deleteAgentMutation = useMutation({
    mutationFn: rdApi.deleteAgentProfile,
    onSuccess: () => {
      message.success(t('rd.configDeleted', '配置已删除'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentProfiles() });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const saveSteeringMutation = useMutation({
    mutationFn: (values: SteeringFormValues) => {
      const payload = {
        repositoryIds: values.repositoryIds ?? [],
        name: values.name,
        description: values.description,
        contentMd: values.contentMd,
        enabled: values.enabled ?? true,
      };
      return editingRule
        ? rdApi.updateSteeringRule(editingRule.id, payload)
        : rdApi.createSteeringRule(payload);
    },
    onSuccess: () => {
      message.success(t('rd.configSaved', '配置已保存'));
      setSteeringModalOpen(false);
      setEditingRule(null);
      steeringForm.resetFields();
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.steeringRules() });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const deleteSteeringMutation = useMutation({
    mutationFn: rdApi.deleteSteeringRule,
    onSuccess: () => {
      message.success(t('rd.configDeleted', '配置已删除'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.steeringRules() });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const saveIntegrationMutation = useMutation({
    mutationFn: (values: IntegrationFormValues) => {
      const payload = {
        provider: values.provider,
        name: values.name,
        configJson: parseConfigJson(values.configText),
        enabled: values.enabled ?? true,
      };
      return editingIntegration
        ? rdApi.updateIntegration(editingIntegration.id, payload)
        : rdApi.createIntegration(payload);
    },
    onSuccess: () => {
      message.success(t('rd.configSaved', '配置已保存'));
      setIntegrationModalOpen(false);
      setEditingIntegration(null);
      integrationForm.resetFields();
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.integrations() });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const deleteIntegrationMutation = useMutation({
    mutationFn: rdApi.deleteIntegration,
    onSuccess: () => {
      message.success(t('rd.configDeleted', '配置已删除'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.integrations() });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const testIntegrationMutation = useMutation({
    mutationFn: rdApi.testIntegration,
    onSuccess: (result) => {
      if (result.ok) {
        message.success(result.message || t('rd.integrationTestPassed', '连接测试通过'));
      } else {
        message.error(result.message || t('rd.integrationTestFailed', '连接测试失败'));
      }
    },
    onError: (error: Error) => message.error(error.message || t('rd.integrationTestFailed', '连接测试失败')),
  });

  const installMarketMutation = useMutation({
    mutationFn: (item: RdAgentMarketItem) => rdApi.installAgentMarketItem(item.id, { enabled: true }),
    onSuccess: (result) => {
      message.success(result.workflow ? t('rd.workflowInstallSuccess', '工作流安装成功') : t('rd.agentInstallSuccess', 'Agent 安装成功'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentMarket({ q: marketQuery, itemType: marketType }) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentProfiles() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentWorkflows() });
      if (result.agentProfile) {
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentProfiles() });
      }
      if (result.workflow) {
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentWorkflows() });
      }
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const uninstallMarketMutation = useMutation({
    mutationFn: async (item: RdAgentMarketItem) => {
      if (!item.installTargetId) throw new Error(t('rd.marketUninstallMissingTarget'));
      if (item.itemType === 'workflow') return rdApi.deleteAgentWorkflow(item.installTargetId);
      return rdApi.deleteAgentProfile(item.installTargetId);
    },
    onSuccess: () => {
      message.success(t('rd.marketUninstallSuccess'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentMarket({ q: marketQuery, itemType: marketType }) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentProfiles() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentWorkflows() });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const saveWorkflowMutation = useMutation({
    mutationFn: (values: WorkflowFormValues) => {
      const payload = {
        name: values.name,
        description: values.description,
        definitionJson: parseWorkflowDefinition(values.definitionText),
        enabled: values.enabled ?? true,
      };
      return editingWorkflow
        ? rdApi.updateAgentWorkflow(editingWorkflow.id, payload)
        : rdApi.createAgentWorkflow(payload);
    },
    onSuccess: () => {
      message.success(t('rd.configSaved', '配置已保存'));
      setWorkflowModalOpen(false);
      setEditingWorkflow(null);
      workflowForm.resetFields();
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentWorkflows() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentMarket({ q: marketQuery, itemType: marketType }) });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  const deleteWorkflowMutation = useMutation({
    mutationFn: rdApi.deleteAgentWorkflow,
    onSuccess: () => {
      message.success(t('rd.configDeleted', '配置已删除'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentWorkflows() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.agentMarket({ q: marketQuery, itemType: marketType }) });
    },
    onError: (error: Error) => message.error(error.message || t('common.operationFailed')),
  });

  function openAgentModal(agent?: RdAgentProfile) {
    setEditingAgent(agent ?? null);
    agentForm.setFieldsValue(agent ? {
      name: agent.name,
      rolePrompt: agent.rolePrompt,
      defaultModel: agent.defaultModel ?? undefined,
      allowedToolsText: prettyJson(agent.allowedTools),
      enabled: agent.enabled,
    } : {
      enabled: true,
      rolePrompt: t('rd.defaultAgentRolePrompt', '你是团队级 Coding Agent。先理解需求和代码结构，再给出可审查的计划、Diff、测试建议和风险说明。'),
    });
    setAgentModalOpen(true);
  }

  function openSteeringModal(rule?: RdSteeringRule) {
    setEditingRule(rule ?? null);
    steeringForm.resetFields();
    steeringForm.setFieldsValue(rule ? {
      repositoryIds: rule.repositoryIds?.length ? rule.repositoryIds : (rule.repositoryId ? [rule.repositoryId] : []),
      name: rule.name,
      description: rule.description ?? undefined,
      contentMd: rule.contentMd,
      enabled: rule.enabled,
    } : {
      enabled: true,
      repositoryIds: [],
    });
    setSteeringModalOpen(true);
  }

  function openSteeringTemplate(template: SteeringTemplate) {
    setEditingRule(null);
    steeringForm.resetFields();
    steeringForm.setFieldsValue({
      repositoryIds: [],
      name: template.name,
      description: template.description,
      contentMd: template.contentMd,
      enabled: true,
    });
    setSteeringModalOpen(true);
  }

  function openIntegrationModal(integration?: RdIntegration) {
    setEditingIntegration(integration ?? null);
    integrationForm.setFieldsValue(integration ? {
      provider: integration.provider,
      name: integration.name,
      configText: prettyJson(integration.configJson),
      enabled: integration.enabled,
    } : {
      provider: 'github',
      enabled: true,
      configText: integrationConfigPlaceholder('github'),
    });
    setIntegrationModalOpen(true);
  }

  function openWorkflowModal(workflow?: RdAgentWorkflow) {
    setEditingWorkflow(workflow ?? null);
    workflowForm.setFieldsValue(workflow ? {
      name: workflow.name,
      description: workflow.description ?? undefined,
      definitionText: prettyJson(workflow.definitionJson),
      enabled: workflow.enabled,
    } : {
      enabled: true,
      definitionText: defaultWorkflowDefinition(),
    });
    setWorkflowModalOpen(true);
  }

  function repoName(repositoryId?: string | null) {
    if (!repositoryId) return t('rd.globalRule', '全局规则');
    const repo = repositoryById.get(repositoryId) as RdRepository | undefined;
    return repo ? `${repo.name} · ${repo.branch}` : repositoryId;
  }

  function ruleRepositoryIds(rule: RdSteeringRule) {
    return rule.repositoryIds?.length ? rule.repositoryIds : (rule.repositoryId ? [rule.repositoryId] : []);
  }

  function renderRuleScope(rule: RdSteeringRule) {
    const repositoryIds = ruleRepositoryIds(rule);
    if (repositoryIds.length === 0) {
      return <Tag color="gold">{t('rd.globalRule', '全局规则')}</Tag>;
    }
    const visible = repositoryIds.slice(0, 3);
    return (
      <Space wrap size={[4, 4]}>
        {visible.map((repositoryId) => <Tag key={repositoryId}>{repoName(repositoryId)}</Tag>)}
        {repositoryIds.length > visible.length ? <Tag>+{repositoryIds.length - visible.length}</Tag> : null}
      </Space>
    );
  }

  function marketTypeLabel(type?: string) {
    if (type === 'workflow') return t('rd.workflow', '工作流');
    if (type === 'agent') return t('rd.agent', 'Agent');
    return t('common.all', '全部');
  }

  return (
    <div style={{ padding: 24, minHeight: '100%', background: 'var(--bg-void)' }}>
      <Space direction="vertical" size={18} style={{ width: '100%' }}>
        <Card>
          <Space align="start" style={{ justifyContent: 'space-between', width: '100%' }}>
            <Space direction="vertical" size={4}>
              <Title level={4} style={{ margin: 0 }}>{t('rd.configTitle', '研发配置')}</Title>
              <Text type="secondary">
                {t('rd.configSubtitle', '管理自定义 Coding Agent 与团队 Steering 规范，任务执行时会自动注入到研发场景。')}
              </Text>
            </Space>
            <Tooltip title={t('rd.configOverviewHelp')}>
              <Button
                type="text"
                shape="circle"
                icon={<QuestionCircleOutlined />}
                aria-label={t('rd.configOverviewHelp')}
                onClick={() => setHelpModal('overview')}
              />
            </Tooltip>
          </Space>
        </Card>

        <Tabs
          // The first release exposes only team steering rules. Agent profiles,
          // workflow marketplace, and external integrations remain available
          // to the backend for later gated releases but are intentionally not
          // presented as configuration the user cannot meaningfully operate.
          items={([
            {
              key: 'market',
              label: <Space><AppstoreOutlined />{t('rd.agentWorkflowMarket', 'Agent / Workflow 市场')}</Space>,
              children: (
                <Card>
                  <Space direction="vertical" size={16} style={{ width: '100%' }}>
	                    <Alert
	                      type="info"
	                      showIcon
	                      message={t('rd.marketFoundationTitle', '精选模板市场')}
	                      description={t('rd.marketFoundationDesc', 'AOS 内置精选 Coding Agent 与多阶段工作流模板；模板由项目维护，不依赖外部仓库扫描，保证质量稳定并可一键安装到当前租户。')}
	                    />
	                    <Space wrap>
	                      <Input.Search
                        allowClear
                        value={marketSearchText}
                        placeholder={t('rd.marketSearchPlaceholder', '搜索 Rust、React、测试修复、Spec、i18n...')}
                        style={{ width: 360 }}
                        onChange={(event) => setMarketSearchText(event.target.value)}
                        onSearch={(value) => setMarketQuery(value.trim())}
                      />
                      <Select
                        allowClear
                        value={marketType}
                        placeholder={t('rd.marketTypeAll', '全部类型')}
                        style={{ width: 160 }}
                        onChange={(value) => setMarketType(value)}
                        options={[
                          { value: 'agent', label: t('rd.agent', 'Agent') },
                          { value: 'workflow', label: t('rd.workflow', '工作流') },
                        ]}
                      />
                      <Select
                        value={marketInstallFilter}
                        style={{ width: 150 }}
                        onChange={setMarketInstallFilter}
                        options={[
                          { value: 'all', label: t('rd.marketInstallFilterAll') },
                          { value: 'installed', label: t('rd.marketInstallFilterInstalled') },
                          { value: 'not_installed', label: t('rd.marketInstallFilterNotInstalled') },
                        ]}
                      />
                      <Button onClick={() => setMarketQuery(marketSearchText.trim())}>
                        {t('common.search')}
                      </Button>
                    </Space>
                    <Table<RdAgentMarketItem>
                      rowKey="id"
                      loading={marketQueryResult.isLoading || marketQueryResult.isFetching}
                      dataSource={marketItems}
                      locale={{ emptyText: <Empty description={t('rd.marketEmpty', '暂未找到匹配模板')} /> }}
                      pagination={{ pageSize: 12 }}
                      columns={[
                        {
                          title: t('common.type', '类型'),
                          dataIndex: 'itemType',
                          width: 110,
                          render: (type: string) => (
                            <Tag color={type === 'workflow' ? 'geekblue' : 'cyan'}>{marketTypeLabel(type)}</Tag>
                          ),
                        },
                        {
                          title: t('common.name'),
                          dataIndex: 'name',
                          render: (name: string, row) => (
                            <Space direction="vertical" size={4}>
                              <Space wrap>
                                <Text strong>{name}</Text>
                                {row.installed ? <Tag color="success" icon={<CheckCircleOutlined />}>{t('rd.installed', '已安装')}</Tag> : null}
                              </Space>
                              <Text type="secondary">{row.description}</Text>
                              <Space wrap size={[4, 4]}>
                                {row.tags.map((tag) => <Tag key={tag}>{tag}</Tag>)}
                              </Space>
                            </Space>
                          ),
                        },
                        {
                          title: t('rd.marketSource', '来源'),
                          dataIndex: 'source',
                          width: 110,
                          render: (source: string) => <Tag>{source}</Tag>,
                        },
                        {
                          title: t('common.actions'),
                          width: 150,
                          render: (_, row) => row.installed ? (
                            <Popconfirm
                              title={t('rd.marketUninstallConfirm', { name: row.name })}
                              okText={t('rd.marketUninstall')}
                              cancelText={t('common.cancel')}
                              okButtonProps={{ danger: true }}
                              onConfirm={() => uninstallMarketMutation.mutate(row)}
                            >
                              <Button
                                size="small"
                                danger
                                icon={<DeleteOutlined />}
                                loading={uninstallMarketMutation.isPending && uninstallMarketMutation.variables?.id === row.id}
                              >
                                {t('rd.marketUninstall')}
                              </Button>
                            </Popconfirm>
                          ) : (
                            <Button
                              size="small"
                              type="primary"
                              icon={<PlusOutlined />}
                              loading={installMarketMutation.isPending && installMarketMutation.variables?.id === row.id}
                              onClick={() => installMarketMutation.mutate(row)}
                            >
                              {row.itemType === 'workflow' ? t('rd.installWorkflow', '安装工作流') : t('rd.installAgent', '安装 Agent')}
                            </Button>
                          ),
                        },
                      ]}
                    />
                  </Space>
                </Card>
              ),
            },
            {
              key: 'agents',
              label: <Space><RobotOutlined />{t('rd.agentProfiles', 'Coding Agent')}</Space>,
              children: (
                <Card
                  extra={<Button type="primary" icon={<PlusOutlined />} onClick={() => openAgentModal()}>{t('rd.newAgentProfile', '新建 Agent')}</Button>}
                >
                  {modelOptions.length === 0 ? (
                    <Alert
                      type="warning"
                      showIcon
                      style={{ marginBottom: 16 }}
                      message={t('rd.noModelTitle', '未配置研发聊天模型')}
                      description={t('rd.noModelDesc', '请到 API 密钥管理添加适用场景为研发、类型为聊天模型的可用密钥。')}
                    />
                  ) : null}
                  <Table
                    rowKey="id"
                    loading={profilesQuery.isLoading}
                    dataSource={profilesQuery.data ?? []}
                    pagination={{ pageSize: 10 }}
                    columns={[
                      {
                        title: t('common.name'),
                        dataIndex: 'name',
                        render: (name: string, row) => (
                          <Space>
                            <Text strong>{name}</Text>
                            {row.enabled ? <Tag color="success">{t('common.enable')}</Tag> : <Tag>{t('common.disable')}</Tag>}
                          </Space>
                        ),
                      },
                      {
                        title: t('rd.defaultModel', '默认模型'),
                        dataIndex: 'defaultModel',
                        render: (value?: string | null) => value || <Text type="secondary">{t('common.na')}</Text>,
                      },
                      {
                        title: t('rd.rolePrompt', '角色提示词'),
                        dataIndex: 'rolePrompt',
                        ellipsis: true,
                      },
                      {
                        title: t('common.actions'),
                        width: 150,
                        render: (_, row) => (
                          <Space>
                            <Button size="small" icon={<EditOutlined />} onClick={() => openAgentModal(row)} />
                            <Popconfirm
                              title={t('rd.deleteAgentProfileConfirm', '删除该 Coding Agent？')}
                              okText={t('common.delete')}
                              cancelText={t('common.cancel')}
                              okButtonProps={{ danger: true }}
                              onConfirm={() => deleteAgentMutation.mutate(row.id)}
                            >
                              <Button size="small" danger icon={<DeleteOutlined />} loading={deleteAgentMutation.isPending && deleteAgentMutation.variables === row.id} />
                            </Popconfirm>
                          </Space>
                        ),
                      },
                    ]}
                  />
                </Card>
              ),
            },
            {
              key: 'workflows',
              label: <Space><ThunderboltOutlined />{t('rd.installedWorkflows', '已安装工作流')}</Space>,
              children: (
                <Card
                  extra={<Button type="primary" icon={<PlusOutlined />} onClick={() => openWorkflowModal()}>{t('rd.newWorkflow', '新建工作流')}</Button>}
                >
                  <Alert
                    type="info"
                    showIcon
                    style={{ marginBottom: 16 }}
                    message={t('rd.workflowRuntimeReadyTitle', '工作流可用于 代码开发任务')}
                    description={t('rd.workflowRuntimeReadyDesc', '代码任务可选择已启用工作流。当前版本会执行最多 2 个前置分析阶段，再把阶段输出注入主 Coding Agent；主修改仍走 Diff-first 审批。')}
                  />
                  <Table<RdAgentWorkflow>
                    rowKey="id"
                    loading={workflowsQuery.isLoading}
                    dataSource={workflowsQuery.data ?? []}
                    locale={{ emptyText: <Empty description={t('rd.noInstalledWorkflows', '暂无已安装工作流，请先到市场安装。')} /> }}
                    pagination={{ pageSize: 10 }}
                    columns={[
                      {
                        title: t('common.name'),
                        dataIndex: 'name',
                        render: (name: string, row) => (
                          <Space direction="vertical" size={4}>
                            <Space>
                              <Text strong>{name}</Text>
                              {row.enabled ? <Tag color="success">{t('common.enable')}</Tag> : <Tag>{t('common.disable')}</Tag>}
                            </Space>
                            <Text type="secondary">{row.description || t('common.na')}</Text>
                          </Space>
                        ),
                      },
                      {
                        title: t('rd.workflowStages', '阶段'),
                        dataIndex: 'definitionJson',
                        render: (definition: Record<string, unknown>) => {
                          const stages = workflowStages(definition);
                          if (stages.length === 0) return <Text type="secondary">{t('common.na')}</Text>;
                          return (
                            <Space wrap size={[4, 4]}>
                              {stages.map((stage, index) => (
                                <Tag key={`${stage.id}-${index}`} color="blue">
                                  {index + 1}. {stage.agent || stage.id}
                                </Tag>
                              ))}
                            </Space>
                          );
                        },
                      },
                      {
                        title: t('rd.marketSource', '来源'),
                        dataIndex: 'source',
                        width: 120,
                        render: (source: string, row) => <Tag>{row.sourceItemId ? `${source}:${row.sourceItemId}` : source}</Tag>,
                      },
                      {
                        title: t('common.updatedAt', '更新时间'),
                        dataIndex: 'updatedAt',
                        width: 180,
                      },
                      {
                        title: t('common.actions'),
                        width: 150,
                        render: (_, row) => (
                          <Space>
                            <Button size="small" icon={<EditOutlined />} onClick={() => openWorkflowModal(row)} />
                            <Popconfirm
                              title={t('rd.deleteWorkflowConfirm', '删除该工作流？已关联任务会自动解除绑定。')}
                              okText={t('common.delete')}
                              cancelText={t('common.cancel')}
                              okButtonProps={{ danger: true }}
                              onConfirm={() => deleteWorkflowMutation.mutate(row.id)}
                            >
                              <Button size="small" danger icon={<DeleteOutlined />} loading={deleteWorkflowMutation.isPending && deleteWorkflowMutation.variables === row.id} />
                            </Popconfirm>
                          </Space>
                        ),
                      },
                    ]}
                  />
                </Card>
              ),
            },
            {
              key: 'steering',
              label: <Space><SafetyCertificateOutlined />{t('rd.steeringRules', '团队规范')}</Space>,
              children: (
                <Card
                  title={(
                    <Space>
                      <span>{t('rd.steeringRules', '团队规范')}</span>
                      <Tooltip title={t('rd.steeringUsageDesc', '全局规则适用于所有仓库；选择仓库后仅在对应仓库任务中生效。说明字段只给团队成员理解用途，不会默认注入模型。')}>
                        <InfoCircleOutlined style={{ color: 'var(--text-tertiary)' }} />
                      </Tooltip>
                    </Space>
                  )}
                  extra={(
                    <Space>
                      <Dropdown
                        menu={{
                          items: steeringTemplates.map((template) => ({
                            key: template.key,
                            label: (
                              <Space direction="vertical" size={0}>
                                <Text>{template.name}</Text>
                                <Text type="secondary" style={{ fontSize: 12 }}>{template.description}</Text>
                              </Space>
                            ),
                          })),
                          onClick: ({ key }) => {
                            const template = steeringTemplates.find((item) => item.key === key);
                            if (template) openSteeringTemplate(template);
                          },
                        }}
                      >
                        <Button>{t('rd.createFromTemplate', '从模板创建')}</Button>
                      </Dropdown>
                      <Button type="primary" icon={<PlusOutlined />} onClick={() => openSteeringModal()}>{t('rd.newSteeringRule', '新建规范')}</Button>
                    </Space>
                  )}
                >
                  <Table
                    rowKey="id"
                    loading={steeringQuery.isLoading}
                    dataSource={steeringQuery.data ?? []}
                    pagination={{ pageSize: 10 }}
                    columns={[
                      {
                        title: t('common.name'),
                        dataIndex: 'name',
                        render: (name: string, row) => (
                          <Space>
                            <Text strong>{name}</Text>
                            {row.enabled ? <Tag color="success">{t('common.enable')}</Tag> : <Tag>{t('common.disable')}</Tag>}
                          </Space>
                        ),
                      },
                      {
                        title: t('rd.scope', '作用范围'),
                        dataIndex: 'repositoryIds',
                        render: (_, row) => renderRuleScope(row),
                      },
                      {
                        title: t('common.description', '描述'),
                        dataIndex: 'description',
                        ellipsis: true,
                        render: (value?: string | null) => value || <Text type="secondary">{t('common.na')}</Text>,
                      },
                      {
                        title: t('rd.content', '规范内容'),
                        dataIndex: 'contentMd',
                        ellipsis: true,
                      },
                      {
                        title: t('common.actions'),
                        width: 150,
                        render: (_, row) => (
                          <Space>
                            <Button size="small" icon={<EditOutlined />} onClick={() => openSteeringModal(row)} />
                            <Popconfirm
                              title={t('rd.deleteSteeringRuleConfirm', '删除该团队规范？')}
                              okText={t('common.delete')}
                              cancelText={t('common.cancel')}
                              okButtonProps={{ danger: true }}
                              onConfirm={() => deleteSteeringMutation.mutate(row.id)}
                            >
                              <Button size="small" danger icon={<DeleteOutlined />} loading={deleteSteeringMutation.isPending && deleteSteeringMutation.variables === row.id} />
                            </Popconfirm>
                          </Space>
                        ),
                      },
                    ]}
                  />
                </Card>
              ),
            },
            {
              key: 'integrations',
              label: <Space><ApiOutlined />{t('rd.integrations', '外部集成')}</Space>,
              children: (
                <Card
                  extra={<Button type="primary" icon={<PlusOutlined />} onClick={() => openIntegrationModal()}>{t('rd.newIntegration', '新建集成')}</Button>}
                >
                  <Alert
                    type="info"
                    showIcon
                    style={{ marginBottom: 16 }}
                    message={t('rd.integrationSafeModeTitle', '这里是连接配置页，不是推送入口')}
                    description={t('rd.integrationSafeModeDesc', '真正推送入口在 代码开发的任务详情里：打开 PR 草稿，选择外部集成，预览 payload 后点击确认推送。测试连接只做只读验证。')}
                  />
                  <Table
                    rowKey="id"
                    loading={integrationsQuery.isLoading}
                    dataSource={integrationsQuery.data ?? []}
                    pagination={{ pageSize: 10 }}
                    columns={[
                      {
                        title: t('rd.provider', '平台'),
                        dataIndex: 'provider',
                        width: 120,
                        render: (provider: string) => <Tag color="blue">{provider}</Tag>,
                      },
                      {
                        title: t('common.name'),
                        dataIndex: 'name',
                        render: (name: string, row) => (
                          <Space>
                            <Text strong>{name}</Text>
                            {row.enabled ? <Tag color="success">{t('common.enable')}</Tag> : <Tag>{t('common.disable')}</Tag>}
                          </Space>
                        ),
                      },
                      {
                        title: t('rd.integrationConfig', '配置'),
                        dataIndex: 'configJson',
                        ellipsis: true,
                        render: (value?: Record<string, unknown> | null) => (
                          <Text code style={{ fontSize: 12 }}>{prettyJson(value) || '{}'}</Text>
                        ),
                      },
                      {
                        title: t('common.actions'),
                        width: 210,
                        render: (_, row) => (
                          <Space>
                            <Button
                              size="small"
                              icon={<ThunderboltOutlined />}
                              loading={testIntegrationMutation.isPending && testIntegrationMutation.variables === row.id}
                              onClick={() => testIntegrationMutation.mutate(row.id)}
                            >
                              {t('rd.testIntegration', '测试')}
                            </Button>
                            <Button size="small" icon={<EditOutlined />} onClick={() => openIntegrationModal(row)} />
                            <Popconfirm
                              title={t('rd.deleteIntegrationConfirm', '删除该外部集成？')}
                              okText={t('common.delete')}
                              cancelText={t('common.cancel')}
                              okButtonProps={{ danger: true }}
                              onConfirm={() => deleteIntegrationMutation.mutate(row.id)}
                            >
                              <Button size="small" danger icon={<DeleteOutlined />} loading={deleteIntegrationMutation.isPending && deleteIntegrationMutation.variables === row.id} />
                            </Popconfirm>
                          </Space>
                        ),
                      },
                    ]}
                  />
                </Card>
              ),
            },
          ].filter((item) => item.key === 'steering'))}
        />
      </Space>

      <Modal
        title={editingAgent ? t('rd.editAgentProfile', '编辑 Coding Agent') : t('rd.newAgentProfile', '新建 Agent')}
        open={agentModalOpen}
        onCancel={() => setAgentModalOpen(false)}
        onOk={() => agentForm.submit()}
        confirmLoading={saveAgentMutation.isPending}
        okText={t('common.save')}
        cancelText={t('common.cancel')}
        width={760}
      >
        <Form form={agentForm} layout="vertical" onFinish={(values) => saveAgentMutation.mutate(values)}>
          <Form.Item name="name" label={t('common.name')} rules={[{ required: true, message: t('common.required') }]}>
            <Input placeholder={t('rd.agentNamePlaceholder', '例如：安全审查 Agent / 前端修复 Agent')} />
          </Form.Item>
          <Form.Item name="defaultModel" label={t('rd.defaultModel', '默认模型')}>
            <Select allowClear options={modelOptions} placeholder={t('rd.inheritTaskModel', '不设置则使用任务选择的模型')} />
          </Form.Item>
          <Form.Item name="rolePrompt" label={t('rd.rolePrompt', '角色提示词')} rules={[{ required: true, message: t('common.required') }]}>
            <Input.TextArea rows={6} placeholder={t('rd.rolePromptPlaceholder', '描述这个 Agent 的职责、偏好、输出结构和安全边界。')} />
          </Form.Item>
          <Form.Item
            name="allowedToolsText"
            label={(
              <Space size={6}>
                <span>{t('rd.allowedToolsJson', '允许工具 JSON')}</span>
                <Button
                  htmlType="button"
                  type="text"
                  size="small"
                  shape="circle"
                  icon={<QuestionCircleOutlined />}
                  aria-label={t('rd.allowedToolsHelpTitle')}
                  onClick={(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    setHelpModal('allowedTools');
                  }}
                />
              </Space>
            )}
          >
            <Input.TextArea rows={4} placeholder='{"tools":["read_file","edit_file","bash"]}' />
          </Form.Item>
          <Paragraph type="secondary" style={{ marginTop: -12 }}>
            {t('rd.allowedToolsHint', '高级可选项：当前会进入任务提示词和审计事件，后续切换到完整 coding runtime 后会作为真实工具边界消费。')}
          </Paragraph>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked">
            <Switch checkedChildren={t('common.enable')} unCheckedChildren={t('common.disable')} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={editingWorkflow ? t('rd.editWorkflow', '编辑工作流') : t('rd.newWorkflow', '新建工作流')}
        open={workflowModalOpen}
        onCancel={() => setWorkflowModalOpen(false)}
        onOk={() => workflowForm.submit()}
        confirmLoading={saveWorkflowMutation.isPending}
        okText={t('common.save')}
        cancelText={t('common.cancel')}
        width={860}
      >
        <Form form={workflowForm} layout="vertical" onFinish={(values) => saveWorkflowMutation.mutate(values)}>
          <Form.Item name="name" label={t('common.name')} rules={[{ required: true, message: t('common.required') }]}>
            <Input placeholder={t('rd.workflowNamePlaceholder', '例如：Spec 到 PR / 失败测试修复 / 前端 i18n 巡检')} />
          </Form.Item>
          <Form.Item name="description" label={t('common.description', '描述')}>
            <Input.TextArea rows={2} placeholder={t('rd.workflowDescriptionPlaceholder', '描述这个工作流适合什么代码任务、阶段边界和安全策略。')} />
          </Form.Item>
          <Form.Item name="definitionText" label={t('rd.workflowDefinitionJson', '工作流定义 JSON')} rules={[{ required: true, message: t('common.required') }]}>
            <Input.TextArea
              rows={14}
              spellCheck={false}
              style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace' }}
            />
          </Form.Item>
          <Paragraph type="secondary" style={{ marginTop: -12 }}>
            {t('rd.workflowDefinitionHint', '必须包含 stages 数组。每个阶段至少填写 agent，可选 id/mode/goal。当前运行时会最多执行 2 个前置分析阶段，主修改仍由 Coding Agent 生成 Diff。')}
          </Paragraph>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked">
            <Switch checkedChildren={t('common.enable')} unCheckedChildren={t('common.disable')} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={editingRule ? t('rd.editSteeringRule', '编辑团队规范') : t('rd.newSteeringRule', '新建规范')}
        open={steeringModalOpen}
        onCancel={() => setSteeringModalOpen(false)}
        onOk={() => steeringForm.submit()}
        confirmLoading={saveSteeringMutation.isPending}
        okText={t('common.save')}
        cancelText={t('common.cancel')}
        width={780}
      >
        <Form form={steeringForm} layout="vertical" onFinish={(values) => saveSteeringMutation.mutate(values)}>
          <Form.Item
            name="repositoryIds"
            label={(
              <Space size={6}>
                <span>{t('rd.scope', '作用范围')}</span>
                <Tooltip title={t('rd.steeringScopeHelp', '可选择多个仓库；不选择则作为全局规则，在所有 代码开发任务中生效。禁用后的规则不会注入模型。')}>
                  <InfoCircleOutlined style={{ color: 'var(--text-tertiary)' }} />
                </Tooltip>
              </Space>
            )}
          >
            <Select
              mode="multiple"
              allowClear
              options={repositoryOptions}
              placeholder={t('rd.globalRuleHint', '不选择仓库则作为全局研发规范')}
              loading={repositoriesQuery.isLoading}
            />
          </Form.Item>
          <Form.Item name="name" label={t('common.name')} rules={[{ required: true, message: t('common.required') }]}>
            <Input placeholder={t('rd.steeringNamePlaceholder', '例如：提交规范 / 安全红线 / React 约定')} />
          </Form.Item>
          <Form.Item
            name="description"
            label={(
              <Space size={6}>
                <span>{t('common.description', '描述')}</span>
                <Tooltip title={t('rd.steeringDescriptionHelp', '用于解释这条规范解决什么问题、什么时候会用。描述只展示给人看，不会默认注入模型。')}>
                  <InfoCircleOutlined style={{ color: 'var(--text-tertiary)' }} />
                </Tooltip>
              </Space>
            )}
          >
            <Input.TextArea rows={3} placeholder={t('rd.steeringDescriptionPlaceholder', '例如：后端仓库的安全红线，涉及鉴权、SQL、日志脱敏时必须遵守。')} />
          </Form.Item>
          <Form.Item name="contentMd" label={t('rd.content', '规范内容')} rules={[{ required: true, message: t('common.required') }]}>
            <Input.TextArea rows={10} placeholder={t('rd.steeringContentPlaceholder', '用 Markdown 写团队约定，例如命名、目录、测试、安全、PR 描述格式。')} />
          </Form.Item>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked">
            <Switch checkedChildren={t('common.enable')} unCheckedChildren={t('common.disable')} />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={editingIntegration ? t('rd.editIntegration', '编辑外部集成') : t('rd.newIntegration', '新建集成')}
        open={integrationModalOpen}
        onCancel={() => setIntegrationModalOpen(false)}
        onOk={() => integrationForm.submit()}
        confirmLoading={saveIntegrationMutation.isPending}
        okText={t('common.save')}
        cancelText={t('common.cancel')}
        width={820}
      >
        <Form form={integrationForm} layout="vertical" onFinish={(values) => saveIntegrationMutation.mutate(values)}>
          <Form.Item name="provider" label={t('rd.provider', '平台')} rules={[{ required: true, message: t('common.required') }]}>
            <Select
              options={INTEGRATION_PROVIDERS.map((item) => ({
                value: item.value,
                label: t(item.labelKey, item.fallback),
              }))}
              onChange={(value) => {
                if (!editingIntegration) {
                  integrationForm.setFieldValue('configText', integrationConfigPlaceholder(value));
                }
              }}
            />
          </Form.Item>
          <Form.Item name="name" label={t('common.name')} rules={[{ required: true, message: t('common.required') }]}>
            <Input placeholder={t('rd.integrationNamePlaceholder', '例如：主 GitHub / 公司 GitLab / Sentry 生产环境')} />
          </Form.Item>
          <Form.Item
            name="configText"
            label={(
              <Space size={6}>
                <span>{t('rd.integrationConfigJson', '高级配置 JSON')}</span>
                <Button
                  htmlType="button"
                  type="text"
                  size="small"
                  shape="circle"
                  icon={<QuestionCircleOutlined />}
                  aria-label={t('rd.integrationHelpTitle')}
                  onClick={(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    setHelpModal('integration');
                  }}
                />
              </Space>
            )}
          >
            <Input.TextArea
              rows={10}
              placeholder={integrationConfigPlaceholder(selectedIntegrationProvider)}
              spellCheck={false}
              style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace' }}
            />
          </Form.Item>
          <Paragraph type="secondary" style={{ marginTop: -12 }}>
            {t('rd.integrationConfigHint', 'token/secret/password 等敏感字段列表展示会自动脱敏；编辑时保留 ******** 即表示沿用原值。')}
          </Paragraph>
          <Form.Item name="enabled" label={t('common.status')} valuePropName="checked">
            <Switch checkedChildren={t('common.enable')} unCheckedChildren={t('common.disable')} />
          </Form.Item>
        </Form>
      </Modal>
      <Modal
        title={helpModal === 'overview'
          ? t('rd.configOverviewHelpTitle')
          : helpModal === 'allowedTools'
            ? t('rd.allowedToolsHelpTitle')
            : t('rd.integrationHelpTitle')}
        open={helpModal !== null}
        zIndex={1300}
        footer={<Button type="primary" onClick={() => setHelpModal(null)}>{t('common.close')}</Button>}
        onCancel={() => setHelpModal(null)}
        width={760}
      >
        {helpModal === 'overview' ? (
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            {(['market', 'agents', 'workflows', 'steering', 'integrations'] as const).map((key) => (
              <div key={key}>
                <Text strong>{t(`rd.configHelp.${key}.title`)}</Text>
                <Paragraph type="secondary" style={{ margin: '4px 0 0' }}>{t(`rd.configHelp.${key}.description`)}</Paragraph>
              </div>
            ))}
          </Space>
        ) : helpModal === 'allowedTools' ? (
          <Space direction="vertical" size={12} style={{ width: '100%' }}>
            <Paragraph>{t('rd.allowedToolsHelpDescription')}</Paragraph>
            <pre style={{ padding: 12, overflow: 'auto', background: 'var(--bg-elevated)', border: '1px solid var(--border-subtle)', borderRadius: 6 }}>{'{\n  "tools": ["read_file", "grep_search", "glob_search", "git_status", "git_diff", "rd_validate_diff"]\n}'}</pre>
            <Text strong>{t('rd.allowedToolsHelpCatalogTitle')}</Text>
            <Paragraph type="secondary" style={{ whiteSpace: 'pre-line' }}>{t('rd.allowedToolsHelpCatalog')}</Paragraph>
            <Alert type="warning" showIcon message={t('rd.allowedToolsHelpGovernance')} />
          </Space>
        ) : (
          <Space direction="vertical" size={12} style={{ width: '100%' }}>
            <Paragraph>{t('rd.integrationHelpDescription')}</Paragraph>
            <Text strong>{t(`rd.integrationProviders.${selectedIntegrationProvider || 'github'}`)}</Text>
            <pre style={{ padding: 12, overflow: 'auto', background: 'var(--bg-elevated)', border: '1px solid var(--border-subtle)', borderRadius: 6 }}>{integrationConfigPlaceholder(selectedIntegrationProvider)}</pre>
            <Paragraph type="secondary" style={{ whiteSpace: 'pre-line' }}>
              {t(`rd.integrationHelpProviders.${selectedIntegrationProvider || 'github'}`)}
            </Paragraph>
            <Alert type="info" showIcon message={t('rd.integrationHelpTestAndPublish')} />
          </Space>
        )}
      </Modal>
    </div>
  );
}
