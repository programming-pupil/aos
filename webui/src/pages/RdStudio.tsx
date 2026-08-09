import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Card,
  Checkbox,
  Collapse,
  Drawer,
  Empty,
  List,
  Modal,
  Select,
  Segmented,
  Space,
  Spin,
  Tag,
  Tabs,
  Typography,
  message,
} from 'antd';
import {
  BranchesOutlined,
  CheckCircleOutlined,
  CodeOutlined,
  DownloadOutlined,
  ExperimentOutlined,
  FileTextOutlined,
  FolderOpenOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  ReloadOutlined,
  RedoOutlined,
  RocketOutlined,
  SafetyCertificateOutlined,
  ShareAltOutlined,
  StopOutlined,
} from '@ant-design/icons';
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useSearchParams } from '@/router';
import { useTranslation } from 'react-i18next';
import dayjs from 'dayjs';
import { agentOpsApi, apiKeysApi, rdApi } from '@/api';
import type { AgentRuntimeArtifactDetail } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { Markdown } from '@/components/chat';
import { RdTokenRootCauseCard, mergeRdTaskDiagnosticEvents } from '@/components/rd/RdTokenRootCauseCard';
import { usePermissions } from '@/store/permissions';
import type { ApiKeyRecord, RdAgentProfile, RdAgentWorkflow, RdBaselinePolicy, RdCodeIntelLocation, RdFileChange, RdPreviewSession, RdRepository, RdStudioMode, RdTask, RdTaskEvent, RdTaskMode, RdTestRun } from '@/types';
import { isRdFileChangeApplicable } from '@/utils/rdChanges';
import { cleanRdPromptForDisplay } from '@/utils/rdDisplay';
import { AgentTimeline } from './rdStudio/AgentTimeline';
import { CodeChatPanel } from './rdStudio/CodeChatPanel';
import { CommandPalette } from './rdStudio/CommandPalette';
import { ContextCacheUsage } from './rdStudio/ContextCacheUsage';
import { DiffInspector } from './rdStudio/DiffInspector';
import { FilePreview } from './rdStudio/FilePreview';
import { CollapsiblePhase, SectionCard } from './rdStudio/LayoutPrimitives';
import { PlanWorkbench } from './rdStudio/PlanWorkbench';
import { PreviewPanel } from './rdStudio/PreviewPanel';
import { PreviewLogsPanel } from './rdStudio/PreviewLogsPanel';
import { QuickOpenPalette } from './rdStudio/QuickOpenPalette';
import { TestInspector } from './rdStudio/TestInspector';
import { TerminalPanel } from './rdStudio/TerminalPanel';
import { CodeWorkbench } from './rdStudio/CodeWorkbench';
import { WorkspaceSidebar } from './rdStudio/WorkspaceSidebar';
import { groupTimelineEventsByStage, latestContextCacheEvent as selectLatestContextCacheEvent } from './rdStudio/apiMapper';
import { useRdStudioShortcuts } from './rdStudio/hooks';
import { buildRdFollowUpPrompt, buildRdTaskReportMarkdown, formatDurationMs } from './rdStudio/reporting';
import type {
  RdRiskFile,
  RdRiskLevel,
  RdRiskMap,
  RdSharePreviewPayload,
  RdTaskThreadSummary,
  RdTimelineEvent,
  RdTokenUsageRow,
  RdWorkspaceTabKey,
  RuntimeConfigSnapshot,
} from './rdStudio/types';
import {
  CONTEXT_DEPTH_FALLBACKS,
  CONTEXT_PROFILE_FALLBACKS,
  RD_RETRIEVAL_SOURCE_FALLBACKS,
  RD_RISK_LEVEL_COLORS,
  RD_RISK_LEVEL_FALLBACKS,
  RD_SHARE_PREVIEW_MAX_CHARS,
  RD_TASK_EVENT_PAGE_SIZE,
  RD_TOKEN_STAGE_FALLBACKS,
  STATUS_COLORS,
} from './rdStudio/constants';
import { asRuntimeToolCalls, mergeRuntimeToolTimelineEvents, runtimeToolReasonLabel } from './rdStudio/runtimeTimeline';
import {
  asRdTaskMode,
  asRuntimeRecord,
  asRuntimeStringArray,
  buildRdTaskThreadSummaries,
  inferRdTaskMode,
  isRdModel,
  modelLabel,
  repoLabel,
  rdTaskIteration,
  rdTaskThreadId,
  runtimeNumber,
  runtimeNumberArray,
  runtimeRecordArray,
  runtimeStringArray,
} from './rdStudio/utils';

const { Text, Title } = Typography;

function statusTag(status?: string | null, label?: string) {
  if (!status) return null;
  return <Tag color={STATUS_COLORS[status] ?? 'blue'}>{label ?? status}</Tag>;
}

function asRdRiskLevel(value: unknown): RdRiskLevel {
  return value === 'critical' || value === 'high' || value === 'medium' || value === 'low'
    ? value
    : 'low';
}

function latestRdRiskMap(events: RdTaskEvent[]): RdRiskMap | null {
  const sorted = [...events].sort((a, b) => b.id - a.id);
  for (const event of sorted) {
    if (event.stage !== 'risk_map') continue;
    const detail = asRuntimeRecord(event.detailJson);
    if (!detail) continue;
    const files = runtimeRecordArray(detail.files).map((file): RdRiskFile => ({
      path: typeof file.path === 'string' && file.path.trim() ? file.path.trim() : '__overall__',
      riskLevel: asRdRiskLevel(file.riskLevel),
      reasons: runtimeStringArray(file.reasons),
      signals: runtimeStringArray(file.signals),
      lineHints: runtimeNumberArray(file.lineHints),
      additions: runtimeNumber(file.additions) ?? 0,
      deletions: runtimeNumber(file.deletions) ?? 0,
    }));
    return {
      riskLevel: asRdRiskLevel(detail.riskLevel),
      mode: typeof detail.mode === 'string' ? detail.mode : undefined,
      sourceStage: typeof detail.sourceStage === 'string' ? detail.sourceStage : undefined,
      files,
      summary: asRuntimeRecord(detail.summary) ?? undefined,
    };
  }
  return null;
}

function runtimeTokenNumber(record: Record<string, unknown> | null, keys: string[]): number {
  for (const key of keys) {
    const value = record?.[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return 0;
}

function tokenUsageRowFromEvent(event: RdTaskEvent, t: ReturnType<typeof useTranslation>['t']): RdTokenUsageRow | null {
  const detail = asRuntimeRecord(event.detailJson);
  if (!detail) return null;
  const usage = asRuntimeRecord(detail.usage) ?? detail;
  const inputTokens = runtimeTokenNumber(usage, ['inputTokens', 'input_tokens']);
  const outputTokens = runtimeTokenNumber(usage, ['outputTokens', 'output_tokens']);
  const cacheCreationTokens = runtimeTokenNumber(usage, ['cacheCreationTokens', 'cache_creation_tokens', 'cacheCreationInputTokens', 'cache_creation_input_tokens']);
  const cacheReadTokens = runtimeTokenNumber(usage, ['cacheReadTokens', 'cache_read_tokens', 'cacheReadInputTokens', 'cache_read_input_tokens']);
  const totalTokens = runtimeTokenNumber(usage, ['totalTokens', 'total_tokens'])
    || inputTokens + outputTokens + cacheCreationTokens + cacheReadTokens;
  if (totalTokens <= 0) return null;
  const model = typeof usage.model === 'string'
    ? usage.model
    : typeof detail.model === 'string'
      ? detail.model
      : undefined;
  return {
    key: `${event.stage}-${event.id}`,
    stage: event.stage,
    label: t(`rd.tokenStages.${event.stage}`, RD_TOKEN_STAGE_FALLBACKS[event.stage] || event.stage),
    model,
    inputTokens,
    outputTokens,
    cacheCreationTokens,
    cacheReadTokens,
    totalTokens,
  };
}

function buildRdStageTokenUsageRows(events: RdTaskEvent[], t: ReturnType<typeof useTranslation>['t']): RdTokenUsageRow[] {
  const rows = events
    .map((event) => tokenUsageRowFromEvent(event, t))
    .filter((row): row is RdTokenUsageRow => !!row);
  const hasRuntimeUsage = rows.some((row) => row.stage === 'runtime_usage');
  const hasRuntimeCompleted = rows.some((row) => row.stage === 'runtime');
  return rows
    .filter((row) => {
      if (row.stage === 'runtime' && hasRuntimeUsage) return false;
      if (row.stage === 'summary' && (hasRuntimeUsage || hasRuntimeCompleted)) return false;
      return ['context_plan_llm', 'runtime_usage', 'runtime', 'summary'].includes(row.stage);
    })
    .sort((a, b) => {
      const order = ['context_plan_llm', 'runtime_usage', 'runtime', 'summary'];
      return order.indexOf(a.stage) - order.indexOf(b.stage);
    });
}

function latestRuntimeToolGovernance(events: RdTaskEvent[]): Record<string, unknown> | null {
  for (const event of events) {
    if (event.stage === 'runtime_tool_governance') {
      const detail = asRuntimeRecord(event.detailJson);
      if (detail) return detail;
    }
  }
  for (const event of events) {
    if (event.stage === 'runtime' || event.stage === 'candidate_worktree') {
      const detail = asRuntimeRecord(event.detailJson);
      const governance = asRuntimeRecord(detail?.toolGovernance);
      if (governance) return governance;
    }
  }
  return null;
}

function runtimeConfigFromEvents(events: RdTaskEvent[]): RuntimeConfigSnapshot {
  const snapshot: RuntimeConfigSnapshot = { mcpServers: [], skills: [], permissionMode: null };
  for (const event of events) {
    const detail = event.detailJson;
    if (!detail || typeof detail !== 'object') continue;
    const mcpServers = asRuntimeStringArray(detail.mcpServers);
    const skills = asRuntimeStringArray(detail.skills);
    if (mcpServers.length > 0) snapshot.mcpServers = mcpServers;
    if (skills.length > 0) snapshot.skills = skills;
    if (typeof detail.permissionMode === 'string') snapshot.permissionMode = detail.permissionMode;
  }
  return snapshot;
}

function toolSnippet(value?: string) {
  if (!value) return '';
  return value.length > 900 ? `${value.slice(0, 900)}\n...[truncated]` : value;
}

function encodeUtf8ToBase64Url(raw: string): string {
  const bytes = new TextEncoder().encode(raw);
  let binary = '';
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/g, '');
}

function buildRdSharePreviewUrl(payload: RdSharePreviewPayload): string {
  if (typeof window === 'undefined') return '';
  const encoded = encodeURIComponent(encodeUtf8ToBase64Url(JSON.stringify(payload)));
  const next = new URL(window.location.href);
  next.pathname = '/preview/share';
  next.search = `?d=${encoded}`;
  next.hash = '';
  return next.toString();
}

function normalizeRdStudioMode(value?: string | null): RdStudioMode {
  if (value === 'spec' || value === 'plan') return 'spec';
  return 'code';
}

function downloadMarkdown(markdown: string, filename: string) {
  const blob = new Blob([markdown], { type: 'text/markdown;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

export default function RdStudio() {
  const { t } = useTranslation();
  const [searchParams, setSearchParams] = useSearchParams();
  const queryClient = useQueryClient();
  const hasPermission = usePermissions((state) => state.hasPermission);
  const canUseCodeDev = hasPermission('rd_studio:read');
  const canWrite = canUseCodeDev;
  const canApply = canUseCodeDev;
  const canRunCommand = canUseCodeDev;
  const [selectedRepoId, setSelectedRepoId] = useState<string | undefined>();
  const [workspaceRepoIds, setWorkspaceRepoIds] = useState<string[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | undefined>();
  const [baselinePolicy, setBaselinePolicy] = useState<RdBaselinePolicy>('current_worktree');
  const [deepModeEnabled, setDeepModeEnabled] = useState(false);
  const [model, setModel] = useState<string | undefined>();
  const [agentProfileId, setAgentProfileId] = useState<string | undefined>();
  const [workflowId, setWorkflowId] = useState<string | undefined>();
  const [continueFromCurrentTask, setContinueFromCurrentTask] = useState(false);
  const [leftPanelCollapsed, setLeftPanelCollapsed] = useState(false);
  const [studioMode, setStudioMode] = useState<RdStudioMode>(() => {
    const saved = window.localStorage.getItem('aos.rdStudio.mode');
    return normalizeRdStudioMode(saved);
  });
  const [activeWorkspaceTab, setActiveWorkspaceTab] = useState<RdWorkspaceTabKey>('result');
  const [activeInspectorTab, setActiveInspectorTab] = useState<RdWorkspaceTabKey>('diff');
  const [selectedPreviewPath, setSelectedPreviewPath] = useState<string | undefined>();
  const [selectedPreviewPosition, setSelectedPreviewPosition] = useState<{ line?: number; character?: number }>({});
  const [referenceLocations, setReferenceLocations] = useState<RdCodeIntelLocation[]>([]);
  const [activePreviewSession, setActivePreviewSession] = useState<RdPreviewSession | null>(null);
  const [quickOpenMode, setQuickOpenMode] = useState<'file' | 'symbol' | null>(null);
  const [commandPaletteOpen, setCommandPaletteOpen] = useState(false);
  const [fileHistory, setFileHistory] = useState<string[]>([]);
  const [fileHistoryIndex, setFileHistoryIndex] = useState(-1);
  const [approvalDrawerOpen, setApprovalDrawerOpen] = useState(false);
  const [selectedRuntimeArtifact, setSelectedRuntimeArtifact] = useState<{
    sessionId: string;
    artifactId: string;
    label: string;
  } | null>(null);
  const [testCommand, setTestCommand] = useState('');
  const [prDraftOpen, setPrDraftOpen] = useState(false);
  const [prDraftIntegrationId, setPrDraftIntegrationId] = useState<string | undefined>();
  const [initialPrompt, setInitialPrompt] = useState<string | undefined>();

  const repositoriesQuery = useQuery({
    queryKey: queryKeys.rd.repositories(),
    queryFn: rdApi.listRepositories,
  });
  const repositories = useMemo(() => repositoriesQuery.data?.repositories ?? [], [repositoriesQuery.data?.repositories]);
  const repositoryIds = useMemo(() => repositories.map((repo) => repo.id), [repositories]);

  useEffect(() => {
    setWorkspaceRepoIds((current) => {
      const known = current.filter((id) => repositoryIds.includes(id));
      if (known.length > 0 || repositories.length === 0) {
        return known;
      }
      return [repositories[0].id];
    });
  }, [repositories, repositoryIds]);

  useEffect(() => {
    window.localStorage.setItem('aos.rdStudio.mode', studioMode);
  }, [studioMode]);

  useEffect(() => {
    const taskIdFromUrl = searchParams.get('taskId');
    const repoIdFromUrl = searchParams.get('repositoryId');
    const followUpFromUrl = searchParams.get('followUp') === '1';
    const modeFromUrl = searchParams.get('mode');
    const promptFromUrl = searchParams.get('prompt');
    const hasRouteState = !!taskIdFromUrl || !!repoIdFromUrl || searchParams.has('followUp') || !!promptFromUrl;
    if (!hasRouteState && !modeFromUrl) return;

    if (taskIdFromUrl) {
      setSelectedTaskId(taskIdFromUrl);
      setStudioMode('code');
    }
    if (repoIdFromUrl) {
      setWorkspaceRepoIds((current) => current.includes(repoIdFromUrl) ? current : [repoIdFromUrl, ...current]);
      setSelectedRepoId(repoIdFromUrl);
    }
    if (modeFromUrl) {
      setStudioMode(normalizeRdStudioMode(modeFromUrl));
    }
    if (promptFromUrl) {
      setInitialPrompt(promptFromUrl);
      setActiveWorkspaceTab('result');
    }
    if (followUpFromUrl) {
      setContinueFromCurrentTask(true);
      setActiveWorkspaceTab('result');
    }
    setSearchParams({}, { replace: true });
  }, [searchParams, setSearchParams]);

  useEffect(() => {
    if (workspaceRepoIds.length === 0) {
      if (selectedRepoId) {
        setSelectedRepoId(undefined);
      }
      return;
    }
    if (!selectedRepoId || !workspaceRepoIds.includes(selectedRepoId)) {
      setSelectedRepoId(workspaceRepoIds[0]);
    }
  }, [selectedRepoId, workspaceRepoIds]);

  const selectedRepo = repositories.find((repo) => repo.id === selectedRepoId) ?? null;
  const workspaceRepositories = useMemo(
    () => workspaceRepoIds
      .map((id) => repositories.find((repo) => repo.id === id))
      .filter((repo): repo is RdRepository => !!repo),
    [repositories, workspaceRepoIds],
  );
  const worktreeStatusQuery = useQuery({
    queryKey: queryKeys.rd.repositoryWorktreeStatus(selectedRepoId),
    queryFn: () => rdApi.repositoryWorktreeStatus(selectedRepoId!),
    enabled: !!selectedRepoId && !!selectedRepo?.isCloned,
    staleTime: 15_000,
    refetchOnWindowFocus: false,
  });
  const worktreeStatus = worktreeStatusQuery.data;

  useEffect(() => {
    setTestCommand(selectedRepo?.defaultTestCommand ?? '');
  }, [selectedRepo?.defaultTestCommand, selectedRepoId]);

  const apiKeysQuery = useQuery({
    queryKey: queryKeys.apiKeys.list(),
    queryFn: apiKeysApi.list,
  });
  const agentProfilesQuery = useQuery({
    queryKey: queryKeys.rd.agentProfiles(),
    queryFn: rdApi.listAgentProfiles,
  });
  const workflowsQuery = useQuery({
    queryKey: queryKeys.rd.agentWorkflows(),
    queryFn: rdApi.listAgentWorkflows,
  });
  const integrationsQuery = useQuery({
    queryKey: queryKeys.rd.integrations(),
    queryFn: rdApi.listIntegrations,
  });
  const agentProfiles = agentProfilesQuery.data ?? [];
  const enabledAgentProfiles = agentProfiles.filter((profile) => profile.enabled);
  const workflows = workflowsQuery.data ?? [];
  const enabledWorkflows = workflows.filter((workflow) => workflow.enabled);
  const modelOptions = useMemo(() => {
    const seen = new Set<string>();
    return (apiKeysQuery.data?.keys ?? [])
      .filter(isRdModel)
      .filter((key) => {
        const value = key.model || key.name;
        if (seen.has(value)) return false;
        seen.add(value);
        return true;
      })
      .sort((a, b) => (a.priority ?? 100) - (b.priority ?? 100))
      .map((key) => ({ value: key.model || key.name, label: modelLabel(key) }));
  }, [apiKeysQuery.data?.keys]);

  useEffect(() => {
    if (!model && modelOptions.length > 0) {
      setModel(modelOptions[0].value);
    }
  }, [model, modelOptions]);

  const taskListParams = useMemo(() => ({ repositoryId: selectedRepoId, page: 1, perPage: 50 }), [selectedRepoId]);
  const tasksQuery = useQuery({
    queryKey: queryKeys.rd.tasks(taskListParams),
    queryFn: () => rdApi.listTasks(taskListParams),
    refetchInterval: (query) => {
      const tasks = query.state.data?.tasks ?? [];
      return tasks.some((task) => ['queued', 'running'].includes(task.status)) ? 2500 : false;
    },
  });
  const tasks = tasksQuery.data?.tasks ?? [];
  const taskThreads = useMemo(() => buildRdTaskThreadSummaries(tasks), [tasks]);

  const selectedTaskQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.task(selectedTaskId) : ['rd', 'task', 'none'],
    queryFn: () => rdApi.getTask(selectedTaskId!),
    enabled: !!selectedTaskId,
    refetchInterval: (query) => {
      const task = query.state.data;
      return task && ['queued', 'running'].includes(task.status) ? 2500 : false;
    },
  });
  const selectedTaskListRow = selectedTaskId ? tasks.find((task) => task.id === selectedTaskId) : undefined;
  const selectedTaskDetail = selectedTaskQuery.data;
  const selectedTaskListRowHasNewerState = !!selectedTaskListRow && !!selectedTaskDetail
    && (
      selectedTaskListRow.status !== selectedTaskDetail.status
      || dayjs(selectedTaskListRow.updatedAt).isAfter(dayjs(selectedTaskDetail.updatedAt))
    );
  const selectedTask = selectedTaskListRowHasNewerState
    ? selectedTaskListRow
    : selectedTaskDetail ?? selectedTaskListRow ?? null;
  const selectedThreadId = selectedTask ? rdTaskThreadId(selectedTask) : undefined;

  useEffect(() => {
    const repoId = selectedTask?.repositoryId;
    if (!repoId) return;
    setWorkspaceRepoIds((current) => current.includes(repoId) ? current : [repoId, ...current]);
    if (repoId !== selectedRepoId) {
      setSelectedRepoId(repoId);
    }
  }, [selectedRepoId, selectedTask?.repositoryId]);

  useEffect(() => {
    if (!selectedTaskId || !selectedTaskListRow) return;
    const detail = selectedTaskQuery.data;
    const shouldRefreshDetail = !detail
      || selectedTaskListRow.status !== detail.status
      || selectedTaskListRow.updatedAt !== detail.updatedAt;
    if (!shouldRefreshDetail) return;
    void queryClient.invalidateQueries({ queryKey: queryKeys.rd.task(selectedTaskId) });
    void queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId) });
    void queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId) });
    void queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskChanges(selectedTaskId) });
    void queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTests(selectedTaskId) });
  }, [
    queryClient,
    selectedTaskId,
    selectedTaskListRow?.status,
    selectedTaskListRow?.updatedAt,
    selectedTaskQuery.data?.status,
    selectedTaskQuery.data?.updatedAt,
  ]);

  const taskEventsQuery = useInfiniteQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskEvents(selectedTaskId, { perPage: RD_TASK_EVENT_PAGE_SIZE }) : ['rd', 'events', 'none'],
    queryFn: ({ pageParam }) =>
      rdApi.taskEventsPage(selectedTaskId!, {
        cursorBefore: typeof pageParam === 'number' ? pageParam : undefined,
        perPage: RD_TASK_EVENT_PAGE_SIZE,
      }),
    enabled: !!selectedTaskId,
    initialPageParam: null as number | null,
    getNextPageParam: (lastPage) => lastPage.hasMore ? lastPage.nextCursor ?? undefined : undefined,
    refetchInterval: selectedTask && ['queued', 'running'].includes(selectedTask.status) ? 2500 : false,
  });

  const agentOpsTasksQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.agentOps.tasks({
      linked_resource_type: 'rd_task',
      linked_resource_id: selectedTaskId,
      page: 1,
      per_page: 1,
    }) : ['agentOps', 'rdTask', 'none'],
    queryFn: () => agentOpsApi.tasks({
      linked_resource_type: 'rd_task',
      linked_resource_id: selectedTaskId!,
      page: 1,
      per_page: 1,
    }),
    enabled: !!selectedTaskId,
    refetchInterval: selectedTask && ['queued', 'running'].includes(selectedTask.status) ? 2500 : false,
  });
  const selectedAgentOpsTask = agentOpsTasksQuery.data?.items?.[0] ?? null;
  const agentOpsEventsQuery = useQuery({
    queryKey: selectedAgentOpsTask ? queryKeys.agentOps.taskEvents(selectedAgentOpsTask.id) : ['agentOps', 'rdTaskEvents', 'none'],
    queryFn: () => agentOpsApi.taskEvents(selectedAgentOpsTask!.id, { page: 1, per_page: 20 }),
    enabled: !!selectedAgentOpsTask,
    refetchInterval: selectedAgentOpsTask && ['queued', 'claimed', 'running', 'waiting_input', 'retrying'].includes(selectedAgentOpsTask.status) ? 2500 : false,
  });
  const agentOpsEvents = agentOpsEventsQuery.data?.items ?? [];
  const workbenchQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskWorkbench(selectedTaskId) : ['rd', 'workbench', 'none'],
    queryFn: () => rdApi.taskWorkbench(selectedTaskId!),
    enabled: !!selectedTaskId,
    refetchInterval: selectedTask && ['queued', 'running', 'waiting_approval'].includes(selectedTask.status) ? 2500 : false,
  });
  const workbench = workbenchQuery.data ?? null;
  const runtimeArtifactDetailQuery = useQuery({
    queryKey: selectedRuntimeArtifact
      ? queryKeys.agentOps.runtimeArtifact(selectedRuntimeArtifact.sessionId, selectedRuntimeArtifact.artifactId)
      : ['rd', 'runtimeArtifact', 'none'],
    queryFn: () => agentOpsApi.runtimeArtifact(
      selectedRuntimeArtifact!.sessionId,
      selectedRuntimeArtifact!.artifactId,
    ),
    enabled: !!selectedRuntimeArtifact,
  });

  const taskTokenDiagnosticsQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskTokenDiagnostics(selectedTaskId) : ['rd', 'tokenDiagnostics', 'none'],
    queryFn: () => rdApi.taskTokenDiagnostics(selectedTaskId!),
    enabled: !!selectedTaskId,
    refetchInterval: selectedTask && ['queued', 'running'].includes(selectedTask.status) ? 2500 : false,
  });

  const taskChangesQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskChanges(selectedTaskId) : ['rd', 'changes', 'none'],
    queryFn: () => rdApi.taskChanges(selectedTaskId!),
    enabled: !!selectedTaskId,
    refetchInterval: (query) => {
      if (!selectedTask) return false;
      if (['queued', 'running', 'waiting_approval'].includes(selectedTask.status)) return 2500;
      const data = query.state.data ?? [];
      const recentlyCompleted = selectedTask.completedAt
        ? dayjs().diff(dayjs(selectedTask.completedAt), 'second') < 20
        : dayjs().diff(dayjs(selectedTask.updatedAt), 'second') < 20;
      if (selectedTask.mode === 'modify' && selectedTask.status === 'completed' && data.length === 0 && recentlyCompleted) {
        return 2500;
      }
      return false;
    },
  });

  const taskTestsQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.taskTests(selectedTaskId) : ['rd', 'tests', 'none'],
    queryFn: () => rdApi.taskTests(selectedTaskId!),
    enabled: !!selectedTaskId,
  });
  const prDraftQuery = useQuery({
    queryKey: selectedTaskId ? queryKeys.rd.prDraft(selectedTaskId, prDraftIntegrationId) : ['rd', 'prDraft', 'none'],
    queryFn: () => rdApi.taskPrDraft(selectedTaskId!, { integrationId: prDraftIntegrationId }),
    enabled: prDraftOpen && !!selectedTaskId,
  });

  const publishPrDraftMutation = useMutation({
    mutationFn: () => rdApi.publishPrDraft(selectedTaskId!, { integrationId: prDraftIntegrationId! }),
    onSuccess: (result) => {
      if (result.ok) {
        message.success(result.remoteUrl
          ? t('rd.publishPrDraftSuccessWithUrl', '已推送到外部系统：{{url}}', { url: result.remoteUrl })
          : result.message || t('rd.publishPrDraftSuccess', '已推送到外部系统'));
      } else {
        message.error(result.message || t('rd.publishPrDraftFailed', '推送失败'));
      }
      if (selectedTaskId) {
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId) });
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId) });
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.prDraft(selectedTaskId, prDraftIntegrationId) });
      }
    },
    onError: (error: Error) => message.error(error.message || t('rd.publishPrDraftFailed', '推送失败')),
  });

  const createTaskMutation = useMutation({
    mutationFn: async ({
      prompt,
      forceContinueFromCurrentTask = false,
      forceDeepMode = false,
    }: {
      prompt: string;
      forceContinueFromCurrentTask?: boolean;
      forceDeepMode?: boolean;
    }) => {
      const routePrompt = prompt.trim();
      const inferredMode = inferRdTaskMode(routePrompt);
      const useFollowUp = forceContinueFromCurrentTask || (continueFromCurrentTask && !!selectedTask);
      const useDeepMode = forceDeepMode || deepModeEnabled;
      let routedContextProfile: string | undefined;
      let routedContextDepth: string | undefined;
      let routedShouldDeepScan: boolean | undefined;
      let routedMode = useFollowUp && selectedTask
        ? (selectedTask.mode as RdTaskMode)
        : inferredMode;
      if (!useFollowUp) {
        try {
          const result = await rdApi.routeIntent({ prompt: routePrompt, model });
          routedMode = asRdTaskMode(result.mode, inferredMode);
          routedContextProfile = result.profile ?? undefined;
          routedContextDepth = result.depth ?? undefined;
          routedShouldDeepScan = result.shouldDeepScan;
        } catch (error) {
          routedMode = inferredMode;
        }
      }
      if (useFollowUp && selectedTask) {
        routedContextProfile = selectedTask.contextProfile ?? undefined;
        routedContextDepth = selectedTask.contextDepth ?? undefined;
        routedShouldDeepScan = selectedTask.shouldDeepScan;
      }
      if (useDeepMode) {
        routedContextDepth = 'deep';
        routedShouldDeepScan = true;
        if (routedMode === 'review') {
          routedContextProfile = 'deep_review';
        }
      }

      return rdApi.createTask({
        repositoryId: selectedRepoId,
        agentProfileId,
        workflowId,
        parentTaskId: useFollowUp && selectedTask ? selectedTask.id : undefined,
        baselinePolicy,
        mode: routedMode,
        contextProfile: routedContextProfile,
        contextDepth: routedContextDepth,
        shouldDeepScan: routedShouldDeepScan,
        title: useFollowUp && selectedTask ? `${t('rd.followUpTitlePrefix', '继续')}：${selectedTask.title}` : undefined,
        prompt: useFollowUp && selectedTask
          ? buildRdFollowUpPrompt({ task: selectedTask, changes: workbenchAwareChanges, tests: workbenchAwareTests, userPrompt: routePrompt })
          : routePrompt,
        model,
      });
    },
    onSuccess: (task, variables) => {
      const wasFollowUp = variables.forceContinueFromCurrentTask || continueFromCurrentTask;
      message.success(wasFollowUp ? t('rd.followUpTaskCreated', '继续任务已创建') : t('rd.taskCreated', '研发任务已创建'));
      setSelectedTaskId(task.id);
      setContinueFromCurrentTask(false);
      setDeepModeEnabled(false);
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(taskListParams) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.taskCreateFailed', '任务创建失败')),
  });
  const handleSubmitRequirement = useCallback(
    (nextPrompt: string) => createTaskMutation.mutateAsync({ prompt: nextPrompt }),
    [createTaskMutation],
  );
  const handleContinueWithDeepMode = useCallback(() => {
    if (!selectedTask) return;
    void createTaskMutation.mutateAsync({
      prompt: t('rd.deepModeFollowUpPrompt', '请基于上一轮结果进入深度模式继续分析：扩大上下文读取范围，优先补齐未核对的关键文件、调用链和测试证据，并给出更完整结论。'),
      forceContinueFromCurrentTask: true,
      forceDeepMode: true,
    });
  }, [createTaskMutation, selectedTask, t]);

  const syncMutation = useMutation({
    mutationFn: (id: string) => rdApi.syncRepository(id),
    onSuccess: (_res, id) => {
      message.success(t('rd.repoSynced', '仓库同步完成'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositoryWorktreeStatus(id) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.repoSyncFailed', '仓库同步失败')),
  });

  const applyMutation = useMutation({
    mutationFn: (changeIds?: string[]) => rdApi.applyChanges(selectedTaskId!, changeIds),
    onSuccess: (res) => {
      message.success(t('rd.applySuccess', '已应用 {{count}} 个修改', { count: res.applied }));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.task(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskChanges(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskWorkbench(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(taskListParams) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositoryWorktreeStatus(selectedRepoId) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.applyFailed', '应用失败')),
  });

  const rollbackMutation = useMutation({
    mutationFn: (changeIds?: string[]) => rdApi.rollbackChanges(selectedTaskId!, changeIds),
    onSuccess: (res) => {
      message.success(t('rd.rollbackSuccess', '已回滚 {{count}} 个修改', { count: res.rolledBack }));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.task(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskChanges(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskWorkbench(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(taskListParams) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositoryWorktreeStatus(selectedRepoId) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.rollbackFailed', '回滚失败')),
  });

  const applyHunksMutation = useMutation({
    mutationFn: ({ changeId, hunkIndexes }: { changeId: string; hunkIndexes: number[] }) =>
      rdApi.applyHunks(selectedTaskId!, changeId, hunkIndexes),
    onSuccess: (res) => {
      message.success(t('rd.applyHunksSuccess', '已应用 {{count}} 个修改块', { count: res.appliedHunks }));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.task(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskChanges(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskWorkbench(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(taskListParams) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositories() });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.repositoryWorktreeStatus(selectedRepoId) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.applyFailed', '应用失败')),
  });

  const testMutation = useMutation({
    mutationFn: () => rdApi.runTest(selectedTaskId!, testCommand.trim()),
    onSuccess: () => {
      message.success(t('rd.testStarted', '测试命令已执行'));
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTests(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId!) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskWorkbench(selectedTaskId!) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.testFailed', '测试失败')),
  });

  const cancelMutation = useMutation({
    mutationFn: () => rdApi.cancelTask(selectedTaskId!),
    onSuccess: () => {
      message.success(t('rd.cancelSuccess', '任务已取消'));
      if (selectedTaskId) {
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.task(selectedTaskId) });
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskEvents(selectedTaskId) });
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskTokenDiagnostics(selectedTaskId) });
        queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskWorkbench(selectedTaskId) });
      }
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(taskListParams) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.cancelFailed', '取消失败')),
  });

  const retryMutation = useMutation({
    mutationFn: () => rdApi.retryTask(selectedTaskId!),
    onSuccess: (task) => {
      message.success(t('rd.retryStarted', '已创建重试任务'));
      setSelectedTaskId(task.id);
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks(taskListParams) });
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.taskWorkbench(task.id) });
    },
    onError: (error: Error) => message.error(error.message || t('rd.retryFailed', '重试失败')),
  });

  const workbenchAwareChanges = useMemo(() => {
    const directChanges = taskChangesQuery.data ?? [];
    return directChanges.length > 0 ? directChanges : workbench?.fileChanges ?? [];
  }, [taskChangesQuery.data, workbench?.fileChanges]);
  const changes = workbenchAwareChanges;
  const applicableChanges = useMemo(() => changes.filter(isRdFileChangeApplicable), [changes]);
  const hasPendingChanges = applicableChanges.length > 0;
  const hasAppliedChanges = changes.some((change) => change.applied);
  const workbenchAwareTests = useMemo(() => {
    const directTests = taskTestsQuery.data ?? [];
    return directTests.length > 0 ? directTests : workbench?.testRuns ?? [];
  }, [taskTestsQuery.data, workbench?.testRuns]);
  const tests = workbenchAwareTests;
  const workbenchLatestAnswer = workbench?.latestAnswer?.trim() || null;
  const resultAnswerMarkdown = selectedTask?.answerMd?.trim() || workbenchLatestAnswer;
  const resultAnswerFromWorkbench = !selectedTask?.answerMd?.trim() && !!workbenchLatestAnswer;
  const events = taskEventsQuery.data?.pages.flatMap((page) => page.events) ?? [];
  const tokenDiagnosticEvents = useMemo(
    () => mergeRdTaskDiagnosticEvents(events, taskTokenDiagnosticsQuery.data?.events ?? []),
    [events, taskTokenDiagnosticsQuery.data?.events],
  );
  const timelineEvents = useMemo(() => mergeRuntimeToolTimelineEvents(events), [events]);
  const latestContextCacheEvent = useMemo(() => selectLatestContextCacheEvent(tokenDiagnosticEvents), [tokenDiagnosticEvents]);
  const stageTokenUsageRows = useMemo(() => buildRdStageTokenUsageRows(tokenDiagnosticEvents, t), [tokenDiagnosticEvents, t]);
  const stageTokenUsageTotal = useMemo(
    () => stageTokenUsageRows.reduce((sum, row) => sum + row.totalTokens, 0),
    [stageTokenUsageRows],
  );
  const eventStageGroups = useMemo(() => groupTimelineEventsByStage(timelineEvents), [timelineEvents]);
  const riskMap = useMemo(() => latestRdRiskMap(events), [events]);
  const riskByPath = useMemo(
    () => new Map((riskMap?.files ?? []).map((file) => [file.path, file] as const)),
    [riskMap],
  );
  const runtimeToolCalls = useMemo(() => timelineEvents.flatMap((event) => asRuntimeToolCalls(event.detailJson?.toolCalls)), [timelineEvents]);
  const runtimeConfig = useMemo(() => runtimeConfigFromEvents(events), [events]);
  const runtimeToolGovernance = useMemo(() => latestRuntimeToolGovernance(events), [events]);
  const runtimeToolGovernancePlan = asRuntimeRecord(runtimeToolGovernance?.plan) ?? asRuntimeRecord(runtimeToolGovernance?.toolGovernancePlan);
  const runtimeToolGovernanceLevel = typeof runtimeToolGovernance?.level === 'string' ? runtimeToolGovernance.level : 'ok';
  const runtimeToolGovernanceRecommendations = runtimeStringArray(runtimeToolGovernance?.recommendations);
  const runtimeSoftFeedback = asRuntimeRecord(runtimeToolGovernance?.softFeedback);
  const runtimeSoftFeedbackLevel = typeof runtimeSoftFeedback?.level === 'string' ? runtimeSoftFeedback.level : runtimeToolGovernanceLevel;
  const runtimeRepeatedInputsCount = runtimeRecordArray(runtimeToolGovernance?.repeatedInputs).length;
  const runtimeFailedTargetsCount = runtimeRecordArray(runtimeToolGovernance?.failedTargets).length;
  const runtimeEmptyResultsCount = runtimeRecordArray(runtimeToolGovernance?.emptyResults).length;
  const runtimeReadActual = runtimeNumber(runtimeToolGovernance?.readFileCount) ?? 0;
  const runtimeSearchActual = runtimeNumber(runtimeToolGovernance?.searchCount) ?? 0;
  const runtimeReadSuggested = runtimeNumber(runtimeToolGovernancePlan?.suggestedReadFileCount) ?? runtimeNumber(runtimeToolGovernance?.suggestedReadFileCount);
  const runtimeSearchSuggested = runtimeNumber(runtimeToolGovernancePlan?.suggestedSearchCount) ?? runtimeNumber(runtimeToolGovernance?.suggestedSearchCount);
  const runtimeSoftReadThreshold = runtimeNumber(runtimeToolGovernancePlan?.softReadThreshold) ?? runtimeNumber(runtimeToolGovernance?.softReadThreshold);
  const runtimeSoftSearchThreshold = runtimeNumber(runtimeToolGovernancePlan?.softSearchThreshold) ?? runtimeNumber(runtimeToolGovernance?.softSearchThreshold);
  const runtimeGovernanceProfile = typeof runtimeToolGovernancePlan?.profile === 'string'
    ? runtimeToolGovernancePlan.profile
    : typeof runtimeToolGovernance?.profile === 'string'
      ? runtimeToolGovernance.profile
      : undefined;
  const runtimeGovernanceProfileName = typeof runtimeToolGovernancePlan?.profileName === 'string'
    ? runtimeToolGovernancePlan.profileName
    : typeof runtimeToolGovernance?.profileName === 'string'
      ? runtimeToolGovernance.profileName
      : undefined;
  const runtimeGovernanceDepth = typeof runtimeToolGovernancePlan?.depth === 'string'
    ? runtimeToolGovernancePlan.depth
    : typeof runtimeToolGovernance?.depth === 'string'
      ? runtimeToolGovernance.depth
      : undefined;
  const runtimeOverSuggestedRead = runtimeToolGovernance?.overSuggestedReadFile === true;
  const runtimeOverSuggestedSearch = runtimeToolGovernance?.overSuggestedSearch === true;
  const runtimeDeepModeRecommended = runtimeToolGovernance?.deepModeRecommended === true;
  const latestTest = tests[0];
  const canRunTestCommand = !!testCommand.trim();
  const hasRunningTask = !!selectedTask && ['queued', 'running'].includes(selectedTask.status);
  const terminalTimelineStatus = selectedTask && ['failed', 'cancelled', 'completed', 'waiting_approval'].includes(selectedTask.status)
    ? selectedTask.status
    : undefined;
  const canCancelTask = !!selectedTask && ['queued', 'running', 'waiting_approval'].includes(selectedTask.status);
  const canRetryTask = !!selectedTask && ['failed', 'cancelled'].includes(selectedTask.status);
  const taskRepo = repositories.find((repo) => repo.id === selectedTask?.repositoryId) ?? selectedRepo;
  const selectedPromptDisplay = selectedTask ? cleanRdPromptForDisplay(selectedTask.prompt) : '';
  const selectedAgentProfile = agentProfiles.find((profile) => profile.id === (selectedTask?.agentProfileId || agentProfileId)) as RdAgentProfile | undefined;
  const selectedWorkflow = workflows.find((item) => item.id === (selectedTask?.workflowId || workflowId)) as RdAgentWorkflow | undefined;
  const enabledIntegrations = (integrationsQuery.data ?? []).filter((integration) => integration.enabled);
  const riskRecommendation = typeof riskMap?.summary?.recommendation === 'string' ? riskMap.summary.recommendation : undefined;
  const statusLabel = (value?: string | null) => {
    const raw = value?.trim();
    if (!raw) return '';
    return t(`rd.statuses.${raw.toLowerCase()}`, { defaultValue: raw });
  };
  const renderStatusTag = (value?: string | null) => statusTag(value, statusLabel(value));
  const riskLevelLabel = (value: RdRiskLevel) => t(`rd.riskLevels.${value}`, RD_RISK_LEVEL_FALLBACKS[value]);
  const contextProfileLabel = (value?: string | null, explicitName?: string | null) => {
    const raw = value?.trim();
    if (!raw) return t('common.na');
    return explicitName || t(`rd.contextProfiles.${raw}`, CONTEXT_PROFILE_FALLBACKS[raw] || raw);
  };
  const contextDepthLabel = (value?: string | null) => {
    const raw = value?.trim();
    if (!raw) return t('common.na');
    return t(`rd.contextDepths.${raw}`, CONTEXT_DEPTH_FALLBACKS[raw] || raw);
  };

  useEffect(() => {
    if (!selectedTask) return;
    if (selectedPreviewPath) return;
    if (hasPendingChanges) {
      setActiveInspectorTab('diff');
    } else if (hasRunningTask) {
      setActiveWorkspaceTab('timeline');
    } else {
      setActiveWorkspaceTab('result');
    }
  }, [hasPendingChanges, hasRunningTask, selectedPreviewPath, selectedTask?.id]);

  useEffect(() => {
    setSelectedPreviewPath(undefined);
  }, [selectedTask?.id]);

  function handleApply(change?: RdFileChange) {
    if (!selectedTaskId) return;
    if (change && !isRdFileChangeApplicable(change)) {
      message.warning(t('rd.changeNotApplicable', '该 Diff 已失效或包含内部运行时路径，不能应用'));
      return;
    }
    Modal.confirm({
      title: t('rd.confirmApplyTitle', '确认应用代码修改？'),
      content: change
        ? t('rd.confirmApplyOne', '将对 {{file}} 应用该 Diff。请确认你已经审查过修改内容。', { file: change.filePath })
        : t('rd.confirmApplyAll', '将应用当前任务的所有未应用 Diff。请确认你已经审查过修改内容。'),
      okText: t('rd.applyPatch', '应用修改'),
      cancelText: t('common.cancel'),
      okButtonProps: { danger: true },
      onOk: () => applyMutation.mutate(change ? [change.id] : applicableChanges.map((item) => item.id)),
    });
  }

  function handleRollback(change?: RdFileChange) {
    if (!selectedTaskId) return;
    Modal.confirm({
      title: t('rd.confirmRollbackTitle', '确认回滚代码修改？'),
      content: change
        ? t('rd.confirmRollbackOne', '将对 {{file}} 反向应用该 Diff，撤回已应用的修改。', { file: change.filePath })
        : t('rd.confirmRollbackAll', '将按应用时间倒序回滚当前任务所有已应用 Diff。'),
      okText: t('rd.rollbackPatch', '回滚修改'),
      cancelText: t('common.cancel'),
      okButtonProps: { danger: true },
      onOk: () => rollbackMutation.mutate(change ? [change.id] : undefined),
    });
  }

  function handleApplyHunks(change: RdFileChange, hunkIndexes: number[]) {
    if (!selectedTaskId) return;
    if (hunkIndexes.length === 0) {
      message.warning(t('rd.noHunkSelected', '请至少选择一个修改块'));
      return;
    }
    Modal.confirm({
      title: t('rd.confirmApplyHunksTitle', '确认应用选中的修改块？'),
      content: t('rd.confirmApplyHunksDesc', '只会应用当前选中的 hunks，未选中的 hunks 会保留为新的待审批 Diff。'),
      okText: t('rd.applySelectedHunks', '应用选中块'),
      cancelText: t('common.cancel'),
      okButtonProps: { danger: true },
      onOk: () => applyHunksMutation.mutate({ changeId: change.id, hunkIndexes }),
    });
  }

  function handleRunTest() {
    if (!selectedTaskId) return;
    const command = testCommand.trim();
    if (!command) {
      message.warning(t('rd.noTestCommand', '未配置测试命令'));
      return;
    }
    Modal.confirm({
      title: t('rd.confirmRunTestTitle', '确认运行测试命令？'),
      content: command,
      okText: t('rd.runTest', '运行测试'),
      cancelText: t('common.cancel'),
      onOk: () => testMutation.mutate(),
    });
  }

  function focusDiffReview(openDrawer = false) {
    setSelectedPreviewPath(undefined);
    setActiveInspectorTab('diff');
    if (openDrawer) {
      setApprovalDrawerOpen(true);
    }
  }

  function handleSelectWorkbenchFile(
    path: string,
    line?: number,
    character?: number,
    options?: { replaceHistory?: boolean },
  ) {
    setSelectedPreviewPath(path);
    setSelectedPreviewPosition({ line, character });
    setActiveWorkspaceTab('file');
    if (!options?.replaceHistory) {
      setFileHistory((history) => {
        const current = fileHistoryIndex >= 0 ? history.slice(0, fileHistoryIndex + 1) : history;
        if (current[current.length - 1] === path) return current;
        const next = [...current, path].slice(-80);
        setFileHistoryIndex(next.length - 1);
        return next;
      });
    }
  }

  function handleReferences(locations: RdCodeIntelLocation[]) {
    setReferenceLocations(locations);
    setActiveInspectorTab('references');
  }

  function handlePreviewFixWithAgent(prompt: string) {
    if (!prompt.trim()) return;
    void createTaskMutation.mutateAsync({
      prompt,
      forceContinueFromCurrentTask: !!selectedTask,
      forceDeepMode: true,
    });
  }

  function navigateFileHistory(direction: -1 | 1) {
    const nextIndex = fileHistoryIndex + direction;
    if (nextIndex < 0 || nextIndex >= fileHistory.length) return;
    const nextPath = fileHistory[nextIndex];
    setFileHistoryIndex(nextIndex);
    handleSelectWorkbenchFile(nextPath, undefined, undefined, { replaceHistory: true });
  }

  useRdStudioShortcuts({
    enabled: canUseCodeDev,
    hasPendingChanges,
    onApply: handleApply,
    onSelectWorkspaceTab: setActiveWorkspaceTab,
    onSelectInspectorTab: setActiveInspectorTab,
    onQuickOpenFiles: () => setQuickOpenMode('file'),
    onQuickOpenSymbols: () => setQuickOpenMode('symbol'),
    onCommandPalette: () => setCommandPaletteOpen(true),
    onNavigateBack: () => navigateFileHistory(-1),
    onNavigateForward: () => navigateFileHistory(1),
  });

  function renderDiffReviewContent(compact = false) {
    const isDiffLoading = changes.length === 0 && (taskChangesQuery.isLoading || taskChangesQuery.isFetching || workbenchQuery.isLoading || workbenchQuery.isFetching);
    return (
      <DiffInspector
        changes={changes}
        loading={isDiffLoading}
        compact={compact}
        hasPendingChanges={hasPendingChanges}
        hasAppliedChanges={hasAppliedChanges}
        canApply={canApply}
        applyLoading={applyMutation.isPending}
        rollbackLoading={rollbackMutation.isPending}
        rollbackVariables={rollbackMutation.variables}
        applyHunksLoading={applyHunksMutation.isPending}
        applyHunksChangeId={applyHunksMutation.variables?.changeId}
        riskByPath={riskByPath}
        riskLevelLabel={riskLevelLabel}
        onApply={handleApply}
        onRollback={handleRollback}
        onApplyHunks={handleApplyHunks}
      />
    );
  }

  function renderReferencesContent() {
    return referenceLocations.length > 0 ? (
      <List
        size="small"
        dataSource={referenceLocations}
        renderItem={(item) => (
          <List.Item className="rd-code-intel-location">
            <Button type="link" onClick={() => handleSelectWorkbenchFile(item.path)}>
              <Space direction="vertical" size={2} style={{ minWidth: 0, textAlign: 'left' }}>
                <Text className="rd-code-intel-location-path">
                  {item.path}:{Math.max(1, item.line + 1)}
                </Text>
                {item.preview ? <Text className="rd-code-intel-location-preview">{item.preview}</Text> : null}
              </Space>
            </Button>
          </List.Item>
        )}
      />
    ) : (
      <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.referencesEmpty', '在编辑器里选择符号并查找引用')}</span>} />
    );
  }

  function renderPreviewContent() {
    return (
      <PreviewPanel
        repository={taskRepo}
        taskId={selectedTaskId}
        onFixWithAgent={handlePreviewFixWithAgent}
        onSessionChange={setActivePreviewSession}
      />
    );
  }

  function renderInspectorContent() {
    const inspectorItems = [
      {
        key: 'diff',
        label: (
          <Space size={6}>
            <span>{t('rd.workspaceTabDiff', 'Diff')}</span>
            {changes.length > 0 ? <Tag color={hasPendingChanges ? 'warning' : 'success'}>{changes.length}</Tag> : null}
          </Space>
        ),
        children: selectedTask ? renderDiffReviewContent(true) : (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.selectTaskFirst', '请先选择一个代码任务')}</span>} />
        ),
      },
      {
        key: 'tests',
        label: (
          <Space size={6}>
            <span>{t('rd.workspaceTabTests', '测试')}</span>
            {latestTest ? renderStatusTag(latestTest.status) : null}
          </Space>
        ),
        children: selectedTask ? renderTestPanelContent() : (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.selectTaskFirst', '请先选择一个代码任务')}</span>} />
        ),
      },
      {
        key: 'context',
        label: <span>{t('rd.contextPanel', '上下文')}</span>,
        children: renderContextPanelContent(),
      },
      {
        key: 'references',
        label: (
          <Space size={6}>
            <span>{t('rd.references', '引用')}</span>
            {referenceLocations.length > 0 ? <Tag color="blue">{referenceLocations.length}</Tag> : null}
          </Space>
        ),
        children: renderReferencesContent(),
      },
      {
        key: 'preview',
        label: <span>{t('rd.previewDebug', '预览调试')}</span>,
        children: renderPreviewContent(),
      },
    ];
    const activeKey = ['diff', 'tests', 'context', 'references', 'preview'].includes(activeInspectorTab)
      ? activeInspectorTab
      : 'diff';
    return (
      <div className="rd-studio-panel-shell">
        <div className="rd-studio-panel-title">
          <Space size={8}>
            <SafetyCertificateOutlined />
            <Text strong>{t('rd.inspector', 'Inspector')}</Text>
          </Space>
        </div>
        <Tabs
          size="small"
          activeKey={activeKey}
          onChange={(key) => setActiveInspectorTab(key as RdWorkspaceTabKey)}
          items={inspectorItems}
        />
      </div>
    );
  }

  function renderBottomPanelContent() {
    return (
      <div className="rd-studio-bottom-tabs">
        <Tabs
          size="small"
          defaultActiveKey="terminal"
          items={[
            {
              key: 'terminal',
              label: <span>{t('rd.terminal', 'Terminal')}</span>,
              children: (
                <TerminalPanel
                  workbench={workbench}
                  testCommand={testCommand}
                  canRun={!!selectedTaskId && canRunCommand && canRunTestCommand && !testMutation.isPending}
                  loading={testMutation.isPending}
                  onTestCommandChange={setTestCommand}
                  onRunTest={handleRunTest}
                />
              ),
            },
            {
              key: 'previewLogs',
              label: <span>{t('rd.previewLogs', 'Preview Logs')}</span>,
              children: <PreviewLogsPanel sessionId={activePreviewSession?.id} />,
            },
            {
              key: 'runtime',
              label: (
                <Space size={6}>
                  <span>{t('rd.runtimeToolCallsShort', 'Runtime')}</span>
                  {runtimeToolCalls.length > 0 ? <Tag color="blue">{runtimeToolCalls.length}</Tag> : null}
                </Space>
              ),
              children: selectedTask ? renderRuntimeToolsContent() : (
                <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.noRuntimeToolCalls', '暂无 Runtime 工具调用')}</span>} />
              ),
            },
          ]}
        />
      </div>
    );
  }

  function renderResultContent() {
    return (
      <Space direction="vertical" size={12} style={{ width: '100%' }}>
        <Card size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }} title={<span style={{ color: '#e2e8f0' }}>{t('rd.plan', '执行计划')}</span>}>
          {selectedTask?.planMd ? <Markdown>{selectedTask.planMd}</Markdown> : <Text style={{ color: '#94a3b8' }}>{t('rd.planPending', '等待 Agent 生成计划...')}</Text>}
        </Card>
        <Card size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }} title={<span style={{ color: '#e2e8f0' }}>{t('rd.answer', '结果总结')}</span>}>
          {resultAnswerMarkdown ? (
            <Space direction="vertical" size={8} style={{ width: '100%' }}>
              {resultAnswerFromWorkbench ? <Tag color="blue">{t('rd.workbenchFallbackAnswer', '来自 Workbench 聚合结果')}</Tag> : null}
              <Markdown>{resultAnswerMarkdown}</Markdown>
            </Space>
          ) : <Text style={{ color: '#94a3b8' }}>{t('rd.answerPending', '结果生成中...')}</Text>}
        </Card>
        {selectedTask?.reviewMd ? (
          <Card size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }} title={<span style={{ color: '#e2e8f0' }}>{t('rd.review', '代码审查')}</span>}>
            <Markdown relaxed>{selectedTask.reviewMd}</Markdown>
          </Card>
        ) : null}
        {(selectedTask?.prTitle || selectedTask?.prDescription) ? (
          <Card size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }} title={<span style={{ color: '#e2e8f0' }}>{t('rd.prOutput', 'PR 产物')}</span>}>
            <Space direction="vertical" size={8} style={{ width: '100%' }}>
              {selectedTask.prTitle ? <Text style={{ color: '#e2e8f0' }} strong>{selectedTask.prTitle}</Text> : null}
              {selectedTask.prDescription ? <Markdown>{selectedTask.prDescription}</Markdown> : null}
            </Space>
          </Card>
        ) : null}
        {riskMap ? (
          <Alert
            showIcon
            type={riskMap.riskLevel === 'critical' || riskMap.riskLevel === 'high' ? 'warning' : 'info'}
            message={(
              <Space wrap>
                <span>{t('rd.riskMapSummary', 'Review/Modify 风险摘要')}</span>
                <Tag color={RD_RISK_LEVEL_COLORS[riskMap.riskLevel]}>{riskLevelLabel(riskMap.riskLevel)}</Tag>
                <Tag color="blue">{t('rd.riskFiles', '影响文件')}: {riskMap.files.length}</Tag>
              </Space>
            )}
            description={riskRecommendation || t('rd.riskMapDefaultRecommendation', '请结合团队规范、Diff 和测试结果完成最终判断。')}
          />
        ) : null}
      </Space>
    );
  }

  function renderTestPanelContent() {
    return (
      <TestInspector
        tests={tests}
        testCommand={testCommand}
        canRun={!!selectedTaskId && canRunCommand && canRunTestCommand && !testMutation.isPending}
        loading={testMutation.isPending}
        renderStatusTag={renderStatusTag}
        onTestCommandChange={setTestCommand}
        onRunTest={handleRunTest}
      />
    );
  }

  function renderRuntimeArtifactContent(detail?: AgentRuntimeArtifactDetail | null) {
    const content = detail?.content || detail?.contentText || '';
    if (!content) {
      return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.agentRuntimeArtifactEmpty', '暂无可展示内容')} />;
    }
    return (
      <pre
        style={{
          maxHeight: 'calc(100vh - 220px)',
          overflow: 'auto',
          margin: 0,
          padding: 12,
          background: '#020617',
          color: '#dbeafe',
          border: '1px solid rgba(148, 163, 184, 0.22)',
          borderRadius: 6,
          whiteSpace: 'pre-wrap',
          overflowWrap: 'anywhere',
          fontSize: 12,
          lineHeight: 1.6,
        }}
      >
        {content}
      </pre>
    );
  }

  function renderTokenContent() {
    if (stageTokenUsageRows.length === 0) {
      return (
        <Space direction="vertical" size={10} style={{ width: '100%' }}>
          <RdTokenRootCauseCard
            events={tokenDiagnosticEvents}
            loading={taskTokenDiagnosticsQuery.isLoading}
          />
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.noTokenBill', '暂无阶段 Token 账单')}</span>} />
        </Space>
      );
    }
    return (
      <Space direction="vertical" size={10} style={{ width: '100%' }}>
        <RdTokenRootCauseCard
          events={tokenDiagnosticEvents}
          loading={taskTokenDiagnosticsQuery.isLoading}
        />
        <Alert
          type="info"
          showIcon
          message={t('rd.stageTokenBillTitle', '阶段级 Token 消耗')}
          description={t('rd.stageTokenBillDesc', '这里只展示任务事件里的阶段明细，不会额外计费；真实总账仍以系统 token_usage 记录为准。')}
        />
        <Space wrap>
          <Tag color="geekblue">{t('rd.totalTokens', '总 Token')}: {stageTokenUsageTotal.toLocaleString()}</Tag>
          <Tag color="blue">{t('rd.stageCount', '{{count}} 个阶段', { count: stageTokenUsageRows.length })}</Tag>
        </Space>
        {stageTokenUsageRows.map((row) => (
          <Card key={row.key} size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }}>
            <Space direction="vertical" size={6} style={{ width: '100%' }}>
              <Space wrap>
                <Tag color={row.stage === 'context_plan_llm' ? 'purple' : row.stage.startsWith('runtime') ? 'cyan' : 'blue'}>{row.label}</Tag>
                {row.model ? <Tag color="default">{row.model}</Tag> : null}
                <Tag color="geekblue">{t('rd.totalTokens', '总 Token')}: {row.totalTokens.toLocaleString()}</Tag>
              </Space>
              <Space wrap size={[6, 6]}>
                <Tag>{t('rd.inputTokens', '输入')}: {row.inputTokens.toLocaleString()}</Tag>
                <Tag>{t('rd.outputTokens', '输出')}: {row.outputTokens.toLocaleString()}</Tag>
                {row.cacheCreationTokens > 0 ? <Tag color="gold">{t('rd.cacheWriteTokens', '缓存写入')}: {row.cacheCreationTokens.toLocaleString()}</Tag> : null}
                {row.cacheReadTokens > 0 ? <Tag color="green">{t('rd.cacheReadTokens', '缓存读取')}: {row.cacheReadTokens.toLocaleString()}</Tag> : null}
              </Space>
            </Space>
          </Card>
        ))}
      </Space>
    );
  }

  function renderContextPanelContent() {
    if (!selectedTask) {
      return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.selectTaskFirst', '请先选择一个代码任务')}</span>} />;
    }
    return (
      <Space direction="vertical" style={{ width: '100%' }} size={12}>
	        <Card size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }}>
	          <Space direction="vertical" style={{ width: '100%' }} size={12}>
            <Space wrap size={[8, 8]}>
              <Tag color="blue">{t('rd.activeRepository', '当前仓库')}: {taskRepo ? repoLabel(taskRepo) : t('common.na')}</Tag>
              <Tag color="geekblue">{t('rd.activeModel', '当前模型')}: {selectedTask.model || model || t('common.na')}</Tag>
              <Tag color={selectedTask.contextProfile === 'deep_review' || selectedTask.shouldDeepScan ? 'red' : 'purple'}>
                {contextProfileLabel(selectedTask.contextProfile, selectedTask.contextProfileName)}
              </Tag>
              <Tag color={selectedTask.contextDepth === 'deep' ? 'volcano' : selectedTask.contextDepth === 'shallow' ? 'green' : 'blue'}>
                {t('rd.contextDepth', '深度')}: {contextDepthLabel(selectedTask.contextDepth)}
              </Tag>
              {selectedTask.shouldDeepScan ? (
                <Tag color="red">{t('rd.deepScanEnabled', '深度扫描')}</Tag>
              ) : (
                <Tag color="default">{t('rd.progressiveRead', '渐进读取')}</Tag>
              )}
            </Space>
            <div>
              <Text style={{ color: '#94a3b8' }}>{t('rd.activeAgentProfile', '当前 Agent')}</Text>
              <div style={{ color: '#e2e8f0', marginTop: 4 }}>{selectedAgentProfile?.name || t('rd.defaultAgent', '默认 Agent')}</div>
            </div>
            <div>
              <Text style={{ color: '#94a3b8' }}>{t('rd.activeWorkflow', '当前工作流')}</Text>
              <div style={{ color: '#e2e8f0', marginTop: 4 }}>{selectedWorkflow?.name || t('rd.noWorkflow', '未启用工作流')}</div>
            </div>
            {selectedTask.threadId ? (
              <div>
                <Text style={{ color: '#94a3b8' }}>{t('rd.taskThread', '任务线程')}</Text>
                <Space direction="vertical" size={4} style={{ width: '100%', marginTop: 4 }}>
                  <Text style={{ color: '#e2e8f0' }}>{selectedTask.threadTitle || selectedTask.title}</Text>
                  <Text copyable style={{ color: '#93c5fd', fontSize: 12 }}>thread: {selectedTask.threadId}</Text>
                  {selectedTask.parentTaskId ? (
                    <Text copyable style={{ color: '#94a3b8', fontSize: 12 }}>parent: {selectedTask.parentTaskId}</Text>
                  ) : null}
                </Space>
              </div>
            ) : null}
	          </Space>
	        </Card>
        <RdTokenRootCauseCard
          events={tokenDiagnosticEvents}
          loading={taskTokenDiagnosticsQuery.isLoading}
        />
        <Card
          size="small"
          style={{ background: '#07111f', borderColor: 'rgba(34, 197, 94, 0.24)' }}
          title={<span style={{ color: '#e2e8f0' }}>{t('rd.contextCacheUsageTitle', '缓存命中与 Token 节省')}</span>}
          extra={latestContextCacheEvent ? (
            <Text style={{ color: '#64748b', fontSize: 12 }}>
              {dayjs(latestContextCacheEvent.createdAt).format('YYYY-MM-DD HH:mm:ss')}
            </Text>
          ) : null}
        >
          {latestContextCacheEvent ? (
            <ContextCacheUsage event={latestContextCacheEvent} />
          ) : (
            <Empty
              image={Empty.PRESENTED_IMAGE_SIMPLE}
              description={<span style={{ color: '#94a3b8' }}>{t('rd.noContextCacheUsage', '暂无缓存命中记录，任务开始生成上下文后会显示。')}</span>}
            />
          )}
        </Card>
	        {(runtimeConfig.mcpServers.length > 0 || runtimeConfig.skills.length > 0 || runtimeConfig.permissionMode) ? (
          <Card size="small" style={{ background: '#07111f', borderColor: 'rgba(148, 163, 184, 0.2)' }} title={<span style={{ color: '#e2e8f0' }}>{t('rd.runtimeExtensions', 'Runtime 扩展')}</span>}>
            <Space wrap size={[6, 6]}>
              {runtimeConfig.permissionMode ? (
                <Tag color="gold">{t('rd.runtimePermissionMode', '权限模式')}: {runtimeConfig.permissionMode}</Tag>
              ) : null}
              {runtimeConfig.mcpServers.map((name) => (
                <Tag key={`mcp-${name}`} color="blue">MCP: {name}</Tag>
              ))}
              {runtimeConfig.skills.map((name) => (
                <Tag key={`skill-${name}`} color="green">Skill: {name}</Tag>
              ))}
            </Space>
          </Card>
        ) : null}
        <Alert
          type="info"
          showIcon
          message={t('rd.safetyRule', '默认安全策略')}
          description={t('rd.safetyRuleDesc', '先生成计划和 Diff，必须人工确认后才会应用到仓库。运行命令也需要确认。')}
        />
      </Space>
    );
  }

  function renderRuntimeToolsContent() {
    if (runtimeToolCalls.length === 0 && !runtimeToolGovernance) {
      return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.noRuntimeToolCalls', '暂无 Runtime 工具调用')}</span>} />;
    }
    return (
      <Space direction="vertical" size={12} style={{ width: '100%' }}>
        {runtimeToolGovernance ? (
          <Alert
            showIcon
            type={runtimeToolGovernanceLevel === 'correction_needed' ? 'error' : runtimeToolGovernanceLevel === 'deep_mode_suggested' ? 'warning' : runtimeToolGovernanceLevel === 'watch' ? 'info' : 'success'}
            message={t('rd.runtimeToolGovernance', '工具治理摘要')}
            description={(
              <Space direction="vertical" size={8} style={{ width: '100%' }}>
                <Space wrap size={[6, 6]}>
                  {runtimeGovernanceProfile ? (
                    <Tag color={runtimeGovernanceProfile === 'deep_review' ? 'red' : 'geekblue'}>
                      {contextProfileLabel(runtimeGovernanceProfile, runtimeGovernanceProfileName)}
                    </Tag>
                  ) : null}
                  {runtimeGovernanceDepth ? (
                    <Tag color={runtimeGovernanceDepth === 'deep' ? 'volcano' : runtimeGovernanceDepth === 'shallow' ? 'green' : 'blue'}>
                      {t('rd.contextDepth', '深度')}: {contextDepthLabel(runtimeGovernanceDepth)}
                    </Tag>
                  ) : null}
                  <Tag color={runtimeOverSuggestedRead ? 'warning' : 'blue'}>
                    read_file: {runtimeReadActual}{runtimeReadSuggested ? ` / ${runtimeReadSuggested}` : ''}
                  </Tag>
                  <Tag color={runtimeOverSuggestedSearch ? 'warning' : 'cyan'}>
                    search: {runtimeSearchActual}{runtimeSearchSuggested ? ` / ${runtimeSearchSuggested}` : ''}
                  </Tag>
                  {runtimeSoftReadThreshold || runtimeSoftSearchThreshold ? (
                    <Tag color="purple">{t('rd.softThreshold', '软阈值')}: {runtimeSoftReadThreshold ?? '-'} / {runtimeSoftSearchThreshold ?? '-'}</Tag>
                  ) : null}
                  <Tag color={(runtimeNumber(runtimeToolGovernance.failedToolCalls) ?? 0) > 0 ? 'error' : 'green'}>
                    {t('rd.failedTools', '失败')}: {runtimeNumber(runtimeToolGovernance.failedToolCalls) ?? 0}
                  </Tag>
                  <Tag color={(runtimeNumber(runtimeToolGovernance.totalDurationMs) ?? 0) > 0 ? 'geekblue' : 'default'}>
                    {t('rd.totalDuration', '总耗时')}: {formatDurationMs(runtimeNumber(runtimeToolGovernance.totalDurationMs) ?? 0)}
                  </Tag>
                  {runtimeRepeatedInputsCount > 0 ? <Tag color="warning">{t('rd.repeatedInputs', '重复输入')}: {runtimeRepeatedInputsCount}</Tag> : null}
                  {runtimeFailedTargetsCount > 0 ? <Tag color="error">{t('rd.failedTargets', '失败目标')}: {runtimeFailedTargetsCount}</Tag> : null}
                  {runtimeEmptyResultsCount > 0 ? <Tag color="gold">{t('rd.emptyResults', '空结果')}: {runtimeEmptyResultsCount}</Tag> : null}
                </Space>
                {runtimeToolGovernanceRecommendations.length > 0 ? (
                  <div style={{ color: '#cbd5e1', fontSize: 12 }}>
                    {runtimeToolGovernanceRecommendations.slice(0, 4).map((item) => (
                      <div key={item}>- {item}</div>
                    ))}
                  </div>
                ) : (
                  <Text style={{ color: '#cbd5e1', fontSize: 12 }}>
                    {t('rd.runtimeToolGovernanceOk', '当前工具读取节奏正常，未发现明显重复或异常扩张。')}
                  </Text>
                )}
                {(runtimeToolGovernanceLevel === 'deep_mode_suggested' || runtimeDeepModeRecommended) && selectedTask && !hasRunningTask ? (
                  <Button size="small" type="primary" danger loading={createTaskMutation.isPending} onClick={handleContinueWithDeepMode}>
                    {t('rd.continueWithDeepMode', '用深度模式继续')}
                  </Button>
                ) : null}
              </Space>
            )}
          />
        ) : null}
        {runtimeToolCalls.slice(0, 20).map((call, idx) => {
          const reason = runtimeToolReasonLabel(call);
          const attribution = asRuntimeRecord(call.attribution);
          const attributionMatched = attribution?.matched === true;
          const attributionSources = runtimeStringArray(attribution?.sources);
          const liveGovernance = asRuntimeRecord(call.governanceSnapshot);
          const callSoftFeedback = asRuntimeRecord(liveGovernance?.softFeedback);
          const callSoftLevel = typeof callSoftFeedback?.level === 'string' ? callSoftFeedback.level : undefined;
          const repeatedTarget = liveGovernance?.repeatedTarget === true;
          const repeatedInput = liveGovernance?.repeatedInput === true;
          const deepModeRecommended = liveGovernance?.deepModeRecommended === true;
          return (
            <Card
              key={`${call.index ?? idx}-${call.toolName ?? 'tool'}`}
              size="small"
              style={{
                background: '#07111f',
                borderColor: call.isError || callSoftLevel === 'correction_needed'
                  ? 'rgba(248, 113, 113, 0.45)'
                  : repeatedTarget || repeatedInput || deepModeRecommended
                    ? 'rgba(251, 146, 60, 0.45)'
                    : 'rgba(148, 163, 184, 0.2)',
              }}
            >
              <Space direction="vertical" size={6} style={{ width: '100%' }}>
                <Space wrap>
                  <Tag color={call.isError ? 'error' : 'blue'}>{call.toolName || t('rd.unknownTool', '未知工具')}</Tag>
                  {call.source ? <Tag>{call.sourceName ? `${call.source}:${call.sourceName}` : call.source}</Tag> : null}
                  {call.target ? <Tag color="geekblue">{t('rd.toolTarget', '目标')}: {call.target}</Tag> : null}
                  {attributionMatched ? <Tag color="green">{t('rd.retrievalEvidence', '召回证据')}</Tag> : null}
                  {attributionSources.slice(0, 3).map((source) => (
                    <Tag key={`${call.index ?? idx}-${source}`} color={source.startsWith('embedding') ? 'purple' : 'default'}>
                      {t(`rd.retrievalSources.${source}`, RD_RETRIEVAL_SOURCE_FALLBACKS[source] || source)}
                    </Tag>
                  ))}
                  {repeatedTarget ? <Tag color="warning">{t('rd.repeatedTarget', '重复目标')}</Tag> : null}
                  {repeatedInput ? <Tag color="warning">{t('rd.repeatedInput', '重复输入')}</Tag> : null}
                  {deepModeRecommended ? <Tag color="volcano">{t('rd.deepModeSuggested', '建议确认深度模式')}</Tag> : null}
                  <Text style={{ color: '#94a3b8', fontSize: 12 }}>{call.durationMs ?? 0}ms</Text>
                </Space>
                {reason ? <Text style={{ color: '#bae6fd', fontSize: 12 }}>{t('rd.toolReason', '原因')}: {reason}</Text> : null}
                {call.input ? <pre style={{ maxHeight: 140, overflow: 'auto', margin: 0, color: '#cbd5e1', whiteSpace: 'pre-wrap', fontSize: 11 }}>{t('rd.toolInput', '输入')}: {toolSnippet(call.input)}</pre> : null}
                {call.output ? <pre style={{ maxHeight: 180, overflow: 'auto', margin: 0, color: call.isError ? '#fecaca' : '#dbeafe', whiteSpace: 'pre-wrap', fontSize: 11 }}>{t('rd.toolOutput', '输出')}: {toolSnippet(call.output)}</pre> : null}
              </Space>
            </Card>
          );
        })}
      </Space>
    );
  }

  function handleCancelTask() {
    if (!selectedTaskId) return;
    Modal.confirm({
      title: t('rd.confirmCancelTitle', '确认取消任务？'),
      content: t('rd.confirmCancelDesc', '取消后当前任务会进入 cancelled 状态，已生成的事件和 Diff 会保留。'),
      okText: t('rd.cancelTask', '取消任务'),
      cancelText: t('common.cancel'),
      okButtonProps: { danger: true },
      onOk: () => cancelMutation.mutate(),
    });
  }

  function handleRetryTask() {
    if (!selectedTaskId) return;
    Modal.confirm({
      title: t('rd.confirmRetryTitle', '确认重试任务？'),
      content: t('rd.confirmRetryDesc', '系统会复制原任务并带上上一次失败上下文，创建一个新的研发任务重新执行。'),
      okText: t('rd.retryTask', '重试'),
      cancelText: t('common.cancel'),
      onOk: () => retryMutation.mutate(),
    });
  }

  function buildCurrentTaskReport() {
    if (!selectedTask) return '';
    return buildRdTaskReportMarkdown({
      task: selectedTask,
      repository: taskRepo,
      changes,
      tests,
      events,
      runtimeToolCalls,
      runtimeConfig,
    });
  }

  function handleDownloadReport() {
    if (!selectedTask) return;
    const markdown = buildCurrentTaskReport();
    downloadMarkdown(markdown, `rd-task-${selectedTask.id}.md`);
  }

  function handleShareReport() {
    if (!selectedTask) return;
    const markdown = buildCurrentTaskReport();
    const content = markdown.slice(0, RD_SHARE_PREVIEW_MAX_CHARS);
    const shareUrl = buildRdSharePreviewUrl({
      schema: 'aos-rd-task-report-v1',
      title: selectedTask.title || t('rd.taskReport', '研发任务报告'),
      generatedAt: new Date().toISOString(),
      messageId: selectedTask.id,
      taskId: selectedTask.id,
      content,
      truncated: markdown.length > RD_SHARE_PREVIEW_MAX_CHARS,
    });
    if (!shareUrl) {
      message.error(t('rd.shareReportFailed', '打开分享预览失败'));
      return;
    }
    const opened = window.open(shareUrl, '_blank', 'noopener,noreferrer');
    if (!opened) message.warning(t('rd.shareReportPopupBlocked', '浏览器阻止了弹窗，请允许弹窗后重试'));
  }

  function handleDownloadPrDraft() {
    const markdown = prDraftQuery.data?.markdown;
    if (!markdown || !selectedTask) return;
    downloadMarkdown(markdown, `rd-pr-draft-${selectedTask.id}.md`);
  }

  function handleSharePrDraft() {
    const markdown = prDraftQuery.data?.markdown;
    if (!markdown || !selectedTask) return;
    const content = markdown.slice(0, RD_SHARE_PREVIEW_MAX_CHARS);
    const shareUrl = buildRdSharePreviewUrl({
      schema: 'aos-rd-task-report-v1',
      title: prDraftQuery.data?.title || t('rd.prDraft', 'PR 草稿'),
      generatedAt: new Date().toISOString(),
      messageId: `${selectedTask.id}-pr-draft`,
      taskId: selectedTask.id,
      content,
      truncated: markdown.length > RD_SHARE_PREVIEW_MAX_CHARS,
    });
    if (!shareUrl) {
      message.error(t('rd.shareReportFailed', '打开分享预览失败'));
      return;
    }
    const opened = window.open(shareUrl, '_blank', 'noopener,noreferrer');
    if (!opened) message.warning(t('rd.shareReportPopupBlocked', '浏览器阻止了弹窗，请允许弹窗后重试'));
  }

  function handlePublishPrDraft() {
    if (!selectedTaskId || !prDraftQuery.data?.markdown) return;
    if (!prDraftIntegrationId) {
      message.warning(t('rd.selectIntegrationBeforePublish', '请先选择要推送的外部集成'));
      return;
    }
    const integration = enabledIntegrations.find((item) => item.id === prDraftIntegrationId);
    Modal.confirm({
      title: t('rd.confirmPublishPrDraftTitle', '确认推送到外部系统？'),
      content: t(
        'rd.confirmPublishPrDraftDesc',
        '将把当前预览的 PR 草稿推送到 {{name}}。此操作会真实调用远端接口，请确认 payload 内容无误。',
        { name: integration ? `${integration.name} · ${integration.provider}` : prDraftIntegrationId },
      ),
      okText: t('rd.publishPrDraft', '确认推送'),
      cancelText: t('common.cancel'),
      okButtonProps: { danger: true },
      onOk: () => publishPrDraftMutation.mutateAsync(),
    });
  }

  function handleOpenPlanTask(taskId: string) {
    setSelectedTaskId(taskId);
    setStudioMode('code');
    setActiveWorkspaceTab('timeline');
  }

  return (
    <div
      className="rd-studio-page"
    >
      <Space direction="vertical" size={16} className="rd-studio-shell" style={{ width: '100%' }}>
        <Card
          className="rd-workspace-bar"
          styles={{ body: { padding: '14px 18px' } }}
        >
          <Space wrap style={{ justifyContent: 'space-between', width: '100%', alignItems: 'center' }}>
            <Space direction="vertical" size={4} style={{ minWidth: 0 }}>
              <Space size={8} wrap>
                  <Tag color="cyan">AOS Code Studio</Tag>
                <Title level={4} style={{ color: '#f8fafc', margin: 0 }}>
                  {studioMode === 'spec'
                    ? t('rd.specModeTitle', 'Spec · Spec -> Design -> Tasks -> Implementation')
                    : t('rd.codeModeTitle', 'Code · Chat, Diff, Test, Apply')}
                </Title>
              </Space>
              <Text style={{ color: '#94a3b8', fontSize: 13, lineHeight: 1.5 }}>
                {studioMode === 'spec'
                  ? t('rd.specModeDesc', '像 Kiro 一样先写 Spec、Design 和任务清单，确认后逐项执行并标记状态。')
                  : t('rd.codeModeDesc', '像 Claude Code / Codex 一样直接输入需求，Agent 读取代码、生成 Diff、跑测试，主仓修改必须确认。')}
              </Text>
            </Space>
            <Space wrap>
              <Segmented
                value={studioMode}
                onChange={(value) => setStudioMode(value as RdStudioMode)}
                options={[
                  { label: t('rd.codeMode', 'Code'), value: 'code' },
                  { label: t('rd.specMode', 'Spec'), value: 'spec' },
                ]}
              />
              <Button
                size="small"
                type="text"
                icon={leftPanelCollapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />}
                onClick={() => setLeftPanelCollapsed((value) => !value)}
                disabled={studioMode === 'spec'}
                style={{ color: '#cbd5e1' }}
              >
                {leftPanelCollapsed ? t('rd.expandSidebar', '展开侧栏') : t('rd.collapseSidebar', '收起侧栏')}
              </Button>
              <Tag color="green">{t('rd.diffFirstBadge', 'Diff 审批')}</Tag>
              <Tag color="blue">{t('rd.runtimeBadge', 'Runtime')}</Tag>
            </Space>
          </Space>
        </Card>

        {studioMode === 'spec' ? (
          <PlanWorkbench
            repositories={repositories}
            selectedRepoId={selectedRepoId}
            model={model}
            agentProfileId={agentProfileId}
            workflowId={workflowId}
            onSelectRepo={(repoId) => {
              setSelectedRepoId(repoId);
              if (repoId) {
                setWorkspaceRepoIds((current) => current.includes(repoId) ? current : [repoId, ...current]);
              }
            }}
            onOpenTask={handleOpenPlanTask}
          />
        ) : (
        <CodeWorkbench
          collapsed={leftPanelCollapsed}
          inspector={renderInspectorContent()}
          bottom={renderBottomPanelContent()}
          sidebar={(
          <>
            <SectionCard title={<Space><RocketOutlined /> {t('rd.beforeStart', '开始前')}</Space>}>
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <div>
                  <Text style={{ color: '#94a3b8' }}>{t('rd.repositories', '代码仓库')}</Text>
                  {repositories.length === 0 ? (
                    <Empty
                      image={Empty.PRESENTED_IMAGE_SIMPLE}
                      description={<span style={{ color: '#94a3b8' }}>{t('rd.emptyRepos', '先添加一个 Git 仓库')}</span>}
                    />
                  ) : (
                    <Select
                      mode="multiple"
                      value={workspaceRepoIds}
                      loading={repositoriesQuery.isLoading}
                      onChange={(ids) => {
                        setWorkspaceRepoIds(ids);
                        if (ids.length === 0) {
                          setSelectedRepoId(undefined);
                        } else if (!ids.includes(selectedRepoId ?? '')) {
                          setSelectedRepoId(ids[0]);
                        }
                      }}
                      style={{ width: '100%', marginTop: 6 }}
                      maxTagCount="responsive"
                      placeholder={t('rd.noRepoSelected', '未选择仓库')}
                      options={repositories.map((repo) => ({
                        value: repo.id,
                        label: `${repo.name} · ${repo.branch}`,
                      }))}
                    />
                  )}
                  {workspaceRepositories.length > 1 ? (
                    <div style={{ marginTop: 10 }}>
                      <Text style={{ color: '#94a3b8', fontSize: 12 }}>
                        {t('rd.primaryRepository', '主执行仓库')}
                      </Text>
                      <Select
                        value={selectedRepoId}
                        onChange={setSelectedRepoId}
                        style={{ width: '100%', marginTop: 6 }}
                        placeholder={t('rd.primaryRepositoryPlaceholder', '选择 Diff / 测试执行仓库')}
                        options={workspaceRepositories.map((repo) => ({
                          value: repo.id,
                          label: `${repo.name} · ${repo.branch}`,
                        }))}
                      />
                      <Text style={{ color: '#64748b', fontSize: 12, display: 'block', marginTop: 6 }}>
                        {t('rd.primaryRepositoryHint', '当前任务的 Diff 应用、测试命令和候选工作区会落到主执行仓库。')}
                      </Text>
                    </div>
                  ) : null}
                  {selectedRepo ? (
                    <Space wrap size={[6, 6]} style={{ marginTop: 8 }}>
                      <Tag color={selectedRepo.isCloned ? 'success' : 'warning'}>
                        {selectedRepo.isCloned ? t('rd.repoReady', '已同步') : t('rd.repoNotSynced', '待同步')}
                      </Tag>
                      {workspaceRepositories.length > 1 ? (
                        <Tag color="cyan">{t('rd.workspaceRepoCount', '工作区 {{count}} 个项目', { count: workspaceRepositories.length })}</Tag>
                      ) : null}
                      <Tag color="blue">{selectedRepo.indexedFileCount} files</Tag>
                      <Button
                        size="small"
                        icon={<ReloadOutlined spin={syncMutation.isPending && syncMutation.variables === selectedRepo.id} />}
                        loading={syncMutation.isPending && syncMutation.variables === selectedRepo.id}
                        onClick={() => syncMutation.mutate(selectedRepo.id)}
                      >
                        {t('rd.syncRepo', '同步仓库')}
                      </Button>
                    </Space>
                  ) : null}
                  {selectedRepo?.isCloned ? (
                    <div style={{ marginTop: 10 }}>
                      {worktreeStatus?.dirty ? (
                        <Alert
                          type="warning"
                          showIcon
                          style={{ marginBottom: 8 }}
                          message={t('rd.dirtyWorktreeTitle', '检测到当前仓库有未提交变更')}
                          description={(
                            <Space direction="vertical" size={4} style={{ width: '100%' }}>
                              <Text style={{ color: '#92400e', fontSize: 12 }}>
                                {t('rd.dirtyWorktreeDesc', '默认会把这些变更作为当前代码事实读取，但不会把它们归属到本次任务 Diff。')}
                              </Text>
                              <Text style={{ color: '#92400e', fontSize: 12 }}>
                                {t('rd.dirtyWorktreeCount', '{{count}} 个路径', { count: worktreeStatus.dirtyPathCount })}
                                {worktreeStatus.dirtyPathsSample.length > 0 ? ` · ${worktreeStatus.dirtyPathsSample.slice(0, 5).join(', ')}` : ''}
                              </Text>
                            </Space>
                          )}
                        />
                      ) : worktreeStatus ? (
                        <Text style={{ color: '#94a3b8', fontSize: 12, display: 'block', marginBottom: 6 }}>
                          {t('rd.cleanWorktreeHint', '当前仓库工作区干净。')}
                        </Text>
                      ) : null}
                      <Space direction="vertical" size={6} style={{ width: '100%' }}>
                        <Text style={{ color: '#94a3b8', fontSize: 12 }}>
                          {t('rd.gitBaselinePolicy', 'Git 基线策略')}
                        </Text>
                        <Select
                          size="small"
                          value={baselinePolicy}
                          onChange={(value) => setBaselinePolicy(value as RdBaselinePolicy)}
                          style={{ width: '100%' }}
                          options={[
                            {
                              value: 'current_worktree',
                              label: t('rd.baselineCurrentWorktree', '基于当前工作区（推荐）'),
                            },
                            {
                              value: 'head',
                              label: t('rd.baselineHead', '基于 HEAD 干净基线'),
                            },
                          ]}
                        />
                        <Text style={{ color: '#64748b', fontSize: 12 }}>
                          {baselinePolicy === 'current_worktree'
                            ? t('rd.baselineCurrentWorktreeDesc', '读取当前未提交代码作为最新事实；AOS 会在候选区创建基线，只把本次 Agent 新增改动放入审批 Diff。')
                            : t('rd.baselineHeadDesc', '忽略当前未提交变更，从仓库 HEAD 创建候选区；适合想验证干净分支上的修改。')}
                        </Text>
                      </Space>
                    </div>
                  ) : null}
                </div>

                <div>
                  <Text style={{ color: '#94a3b8' }}>{t('rd.selectModel', '选择研发模型')}</Text>
                  {modelOptions.length === 0 ? (
                    <Alert
                      type="warning"
                      showIcon
                      style={{ marginTop: 6 }}
                      message={t('rd.noModelTitle', '未配置研发聊天模型')}
                      description={t('rd.noModelDesc', '请到 API 密钥管理添加 scenario=rd 且类型为聊天模型的可用密钥。')}
                    />
                  ) : (
                    <Select
                      value={model}
                      options={modelOptions}
                      onChange={setModel}
                      style={{ width: '100%', marginTop: 6 }}
                      placeholder={t('rd.selectModel', '选择研发模型')}
                    />
                  )}
                </div>

                <Collapse
                  ghost
                  items={[
                    {
                      key: 'advanced',
                      label: <span style={{ color: '#cbd5e1' }}>{t('rd.optionalEnhancements', 'Agent 设置')}</span>,
                      children: (
                        <Space direction="vertical" size={10} style={{ width: '100%' }}>
                          <div
                            style={{
                              padding: 10,
                              borderRadius: 12,
                              background: deepModeEnabled ? 'rgba(127, 29, 29, 0.18)' : 'rgba(2, 6, 23, 0.30)',
                              border: deepModeEnabled ? '1px solid rgba(248, 113, 113, 0.34)' : '1px solid rgba(148, 163, 184, 0.14)',
                            }}
                          >
                            <Checkbox
                              checked={deepModeEnabled}
                              onChange={(event) => setDeepModeEnabled(event.target.checked)}
                            >
                              <span style={{ color: '#dbeafe' }}>{t('rd.deepMode', '深度模式')}</span>
                            </Checkbox>
                            <div style={{ color: '#94a3b8', fontSize: 12, marginTop: 4, paddingLeft: 24 }}>
                              {t('rd.deepModeDesc', '适合全仓审计、复杂架构梳理或上一轮证据不足时使用；会允许更多读取与搜索，但仍保持先召回、再核对真实文件。')}
                            </div>
                          </div>
                          <Select
                            allowClear
                            value={agentProfileId}
                            loading={agentProfilesQuery.isLoading}
                            onChange={setAgentProfileId}
                            style={{ width: '100%' }}
                            placeholder={t('rd.selectAgentProfile', '选择 Coding Agent')}
                            options={enabledAgentProfiles.map((profile) => ({
                              value: profile.id,
                              label: profile.name,
                            }))}
                          />
                          <Select
                            allowClear
                            value={workflowId}
                            loading={workflowsQuery.isLoading}
                            onChange={setWorkflowId}
                            style={{ width: '100%' }}
                            placeholder={t('rd.selectWorkflow', '选择多 Agent 工作流')}
                            options={enabledWorkflows.map((workflow) => ({
                              value: workflow.id,
                              label: workflow.name,
                            }))}
                          />
                        </Space>
                      ),
                    },
                  ]}
                />
              </Space>
            </SectionCard>

            <SectionCard title={<Space><FileTextOutlined /> {t('rd.taskThreads', '任务线程')}</Space>}>
              {tasksQuery.isLoading ? <Spin /> : tasks.length === 0 ? (
                <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.emptyTasks', '暂无研发任务')}</span>} />
              ) : (
                <List
                  dataSource={taskThreads}
                  style={{ maxHeight: 520, overflow: 'auto' }}
                  renderItem={(thread) => (
                    <List.Item
                      onClick={() => setSelectedTaskId(thread.threadId === selectedThreadId ? undefined : thread.latest.id)}
                      style={{
                        cursor: 'pointer',
                        padding: '10px 12px',
                        marginBottom: 6,
                        borderRadius: 12,
                        border: thread.threadId === selectedThreadId ? '1px solid rgba(96, 165, 250, 0.75)' : '1px solid transparent',
                        background: thread.threadId === selectedThreadId ? 'rgba(59, 130, 246, 0.14)' : 'rgba(15, 23, 42, 0.42)',
                      }}
                    >
                      <List.Item.Meta
                        title={
                          <Space direction="vertical" size={6} style={{ width: '100%' }}>
                            <Space size={6} wrap>
                              <Text style={{ color: '#e2e8f0' }}>{thread.title}</Text>
                              <Tag color="geekblue">{t('rd.threadRounds', '{{count}} 轮', { count: thread.count })}</Tag>
                              {renderStatusTag(thread.latest.status)}
                            </Space>
                            {thread.threadId === selectedThreadId && thread.tasks.length > 1 ? (
                              <Space wrap size={[4, 4]}>
                                {thread.tasks.map((task) => (
                                  <Button
                                    key={task.id}
                                    size="small"
                                    type={task.id === selectedTaskId ? 'primary' : 'text'}
                                    onClick={(event) => {
                                      event.stopPropagation();
                                      setSelectedTaskId((current) => current === task.id ? undefined : task.id);
                                    }}
                                    style={task.id === selectedTaskId ? undefined : { color: '#93c5fd' }}
                                  >
                                    {t('rd.iterationNo', '第 {{count}} 轮', { count: rdTaskIteration(task) })}
                                  </Button>
                                ))}
                              </Space>
                            ) : null}
                          </Space>
                        }
                        description={<Text style={{ color: '#94a3b8', fontSize: 12 }}>{t('rd.codeTask', 'Code task')} · {dayjs(thread.latest.createdAt).format('MM-DD HH:mm')}</Text>}
                      />
                    </List.Item>
                  )}
                />
              )}
            </SectionCard>

            {selectedTask ? (
              <SectionCard title={<Space><FolderOpenOutlined /> {t('rd.workspaceFiles', '文件树')}</Space>}>
                <WorkspaceSidebar workbench={workbench} onSelectFile={handleSelectWorkbenchFile} />
              </SectionCard>
            ) : null}
          </>
          )}
        >
            <CodeChatPanel
              canWrite={canWrite}
              isPending={createTaskMutation.isPending}
              modelOptionsCount={modelOptions.length}
              selectedRepo={selectedRepo}
              workspaceRepositories={workspaceRepositories}
              selectedTask={selectedTask}
              model={model}
              deepModeEnabled={deepModeEnabled}
              continueFromCurrentTask={continueFromCurrentTask}
              selectedAgentProfile={selectedAgentProfile}
              selectedWorkflow={selectedWorkflow}
              initialPrompt={initialPrompt}
              onContinueFromCurrentTaskChange={setContinueFromCurrentTask}
              onSubmit={handleSubmitRequirement}
            />

            {selectedTask ? (
              <>
                <CollapsiblePhase
                  key={`overview-${selectedTask.id}`}
                  title={
                    <Space wrap>
                      <CodeOutlined />
                      <span>{selectedTask.title}</span>
                      {selectedTask.iterationNo && selectedTask.iterationNo > 1 ? <Tag color="geekblue">{t('rd.iterationNo', '第 {{count}} 轮', { count: selectedTask.iterationNo })}</Tag> : null}
                      {selectedTask.parentTaskId ? <Tag color="cyan">{t('rd.followUpTag', '续问')}</Tag> : null}
                      {renderStatusTag(selectedTask.status)}
                    </Space>
                  }
                  extra={
                    <Space wrap>
                      {hasRunningTask ? <Spin size="small" /> : null}
                      {canCancelTask ? (
                        <Button
                          size="small"
                          danger
                          icon={<StopOutlined />}
                          loading={cancelMutation.isPending}
                          onClick={handleCancelTask}
                        >
                          {t('rd.cancelTask', '取消任务')}
                        </Button>
                      ) : null}
                      {canRetryTask ? (
                        <Button
                          size="small"
                          icon={<RedoOutlined />}
                          loading={retryMutation.isPending}
                          onClick={handleRetryTask}
                        >
                          {t('rd.retryTask', '重试')}
                        </Button>
                      ) : null}
                      <Button size="small" icon={<DownloadOutlined />} onClick={handleDownloadReport}>
                        {t('rd.downloadReport', '下载报告')}
                      </Button>
                      <Button size="small" icon={<BranchesOutlined />} onClick={() => setPrDraftOpen(true)}>
                        {t('rd.prDraft', 'PR 草稿')}
                      </Button>
                      <Button size="small" icon={<ShareAltOutlined />} onClick={handleShareReport}>
                        {t('rd.shareReport', '分享')}
                      </Button>
                    </Space>
                  }
                  defaultOpen
	                >
	                  {selectedPromptDisplay ? (
	                    <div className="rd-task-requirement-markdown">
	                      <Markdown suppressHr>{selectedPromptDisplay}</Markdown>
                    </div>
                  ) : (
                    <Text style={{ color: '#94a3b8' }}>{t('rd.emptyDisplayPrompt', '需求内容为空或仅包含分隔符')}</Text>
                  )}
	                  {selectedTask.errorMessage ? <Alert type="error" message={selectedTask.errorMessage} style={{ marginTop: 12 }} /> : null}
	                </CollapsiblePhase>

	                {changes.length > 0 ? (
	                  <Card
	                    size="small"
	                    style={{
	                      background: hasPendingChanges ? 'linear-gradient(135deg, rgba(120, 53, 15, 0.62), rgba(15, 23, 42, 0.88))' : 'rgba(15, 23, 42, 0.72)',
	                      borderColor: hasPendingChanges ? 'rgba(251, 191, 36, 0.42)' : 'rgba(34, 197, 94, 0.28)',
	                      boxShadow: hasPendingChanges ? '0 18px 48px rgba(120, 53, 15, 0.24)' : undefined,
	                    }}
	                  >
	                    <Space wrap style={{ justifyContent: 'space-between', width: '100%', gap: 12 }}>
	                      <Space direction="vertical" size={4}>
	                        <Space wrap>
	                          <SafetyCertificateOutlined style={{ color: hasPendingChanges ? '#facc15' : '#86efac' }} />
	                          <Text style={{ color: '#f8fafc', fontWeight: 700 }}>
	                            {hasPendingChanges
	                              ? t('rd.diffCalloutTitle', 'Agent 已生成待审批 Diff')
	                              : t('rd.diffCalloutDoneTitle', '本轮 Diff 已处理')}
	                          </Text>
	                          <Tag color={hasPendingChanges ? 'warning' : 'success'}>{t('rd.changeCount', '{{count}} 个变更', { count: changes.length })}</Tag>
	                          {hasPendingChanges ? <Tag color="volcano">{t('rd.pendingApplyCount', '{{count}} 个待应用', { count: applicableChanges.length })}</Tag> : null}
	                        </Space>
	                        <Text style={{ color: '#cbd5e1', fontSize: 12 }}>
	                          {t('rd.diffCalloutDesc', '不用去代码任务详情，直接在当前工作区审查、应用、运行测试。快捷键：D=Diff，R=结果，T=测试，L=时间线，P=Token，⌘/Ctrl+Enter=应用确认。')}
	                        </Text>
	                      </Space>
	                      <Space wrap>
	                        <Button size="small" onClick={() => focusDiffReview(false)}>{t('rd.viewDiff', '查看 Diff')}</Button>
	                        <Button size="small" onClick={() => focusDiffReview(true)}>{t('rd.openApprovalDrawer', '浮窗审批')}</Button>
	                        <Button
	                          size="small"
	                          danger
	                          type={hasPendingChanges ? 'primary' : 'default'}
	                          disabled={!canApply || !hasPendingChanges || applyMutation.isPending}
	                          loading={applyMutation.isPending}
	                          onClick={() => handleApply()}
	                        >
	                          {t('rd.applyAll', '应用全部')}
	                        </Button>
	                        <Button
	                          size="small"
	                          icon={<ExperimentOutlined />}
	                          disabled={!selectedTaskId || !canRunCommand || !canRunTestCommand || testMutation.isPending}
	                          loading={testMutation.isPending}
	                          onClick={handleRunTest}
	                        >
	                          {t('rd.runTest', '运行测试')}
	                        </Button>
	                      </Space>
	                    </Space>
	                  </Card>
	                ) : null}

		                <Card
		                  size="small"
		                  className="rd-studio-primary-tabs"
		                  styles={{ body: { padding: 10 } }}
		                >
		                  <Tabs
		                    activeKey={activeWorkspaceTab}
		                    onChange={(key) => setActiveWorkspaceTab(key as RdWorkspaceTabKey)}
		                    items={[
	                      {
	                        key: 'result',
	                        label: <span>{t('rd.workspaceTabResult', '结果')}</span>,
	                        children: renderResultContent(),
	                      },
	                      {
	                        key: 'file',
	                        label: (
	                          <Space size={6}>
	                            <span>{t('rd.workspaceTabFile', '文件')}</span>
	                            {selectedPreviewPath ? <Tag color="cyan">{selectedPreviewPath.split('/').pop()}</Tag> : null}
	                          </Space>
	                        ),
		                        children: (
                            <FilePreview
                              repository={taskRepo}
                              path={selectedPreviewPath}
                              revealLine={selectedPreviewPosition.line}
                              revealColumn={selectedPreviewPosition.character}
                              onOpenPath={handleSelectWorkbenchFile}
                              onReferences={handleReferences}
                            />
                          ),
		                      },
		                      {
		                        key: 'timeline',
	                        label: (
	                          <Space size={6}>
	                            <span>{t('rd.workspaceTabTimeline', '时间线')}</span>
	                            {hasRunningTask ? <Spin size="small" /> : null}
	                          </Space>
	                        ),
	                        children: (
                            <AgentTimeline
                              task={selectedAgentOpsTask ?? workbench?.agentTask ?? null}
                              opsEvents={agentOpsEvents}
                              workbench={workbench}
                              events={events}
                              eventStageGroups={eventStageGroups}
                              terminalTimelineStatus={terminalTimelineStatus}
                              hasNextPage={taskEventsQuery.hasNextPage}
                              isFetchingNextPage={taskEventsQuery.isFetchingNextPage}
                              renderStatusTag={renderStatusTag}
                              canCancelTask={canCancelTask}
                              canRetryTask={canRetryTask}
                              cancelLoading={cancelMutation.isPending}
                              retryLoading={retryMutation.isPending}
                              onCancel={handleCancelTask}
                              onRetry={handleRetryTask}
                              onReviewDiff={() => focusDiffReview(true)}
                              onShowTests={() => setActiveInspectorTab('tests')}
                              onOpenArtifact={setSelectedRuntimeArtifact}
                              onLoadMore={() => {
                                void taskEventsQuery.fetchNextPage();
                              }}
                            />
		                          ),
		                      },
		                      {
		                        key: 'tokens',
	                        label: (
	                          <Space size={6}>
	                            <span>{t('rd.workspaceTabTokens', 'Token')}</span>
	                            {stageTokenUsageTotal > 0 ? <Tag color="blue">{stageTokenUsageTotal.toLocaleString()}</Tag> : null}
	                          </Space>
	                        ),
	                        children: renderTokenContent(),
	                      },
	                    ]}
		                  />
		                </Card>

	              </>
            ) : null}
        </CodeWorkbench>
        )}
      </Space>

      <Drawer
        title={(
          <Space>
            <SafetyCertificateOutlined />
            <span>{t('rd.diffApprovalDrawerTitle', 'Diff 浮窗审批')}</span>
            {hasPendingChanges ? <Tag color="warning">{t('rd.pendingApplyCount', '{{count}} 个待应用', { count: applicableChanges.length })}</Tag> : null}
          </Space>
        )}
        open={approvalDrawerOpen}
        onClose={() => setApprovalDrawerOpen(false)}
        width={980}
        styles={{
          body: { background: '#020617' },
          header: { background: '#07111f', borderBottomColor: 'rgba(148, 163, 184, 0.18)' },
        }}
      >
        {selectedTask ? renderDiffReviewContent(true) : (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={<span style={{ color: '#94a3b8' }}>{t('rd.selectTaskFirst', '请先选择一个代码任务')}</span>}
          />
        )}
      </Drawer>

      <Drawer
        title={(
          <Space direction="vertical" size={2} style={{ maxWidth: '100%' }}>
            <Space size={8} wrap>
              <FileTextOutlined />
              <span>{t('rd.agentRuntimeArtifactDetail', '运行产物内容')}</span>
              {runtimeArtifactDetailQuery.data?.artifactType ? (
                <Tag color="geekblue">{runtimeArtifactDetailQuery.data.artifactType}</Tag>
              ) : null}
              {runtimeArtifactDetailQuery.data?.sizeBytes !== undefined ? (
                <Tag>{runtimeArtifactDetailQuery.data.sizeBytes}B</Tag>
              ) : null}
            </Space>
            <Text
              type="secondary"
              ellipsis={{ tooltip: selectedRuntimeArtifact?.label }}
              style={{ maxWidth: 760, fontSize: 12 }}
            >
              {selectedRuntimeArtifact?.label}
            </Text>
          </Space>
        )}
        open={!!selectedRuntimeArtifact}
        onClose={() => setSelectedRuntimeArtifact(null)}
        width={900}
        styles={{
          body: { background: '#020617' },
          header: { background: '#07111f', borderBottomColor: 'rgba(148, 163, 184, 0.18)' },
        }}
      >
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          {runtimeArtifactDetailQuery.error ? (
            <Alert
              type="error"
              showIcon
              message={t('rd.agentRuntimeArtifactLoadFailed', '运行产物加载失败')}
              description={(runtimeArtifactDetailQuery.error as Error).message}
            />
          ) : null}
          {runtimeArtifactDetailQuery.data?.contentTruncated ? (
            <Alert type="warning" showIcon message={t('rd.agentRuntimeArtifactTruncated', '内容较长，已展示安全长度内的前半部分。')} />
          ) : null}
          {runtimeArtifactDetailQuery.isLoading ? (
            <Spin />
          ) : (
            renderRuntimeArtifactContent(runtimeArtifactDetailQuery.data)
          )}
        </Space>
      </Drawer>

      <Modal
        title={t('rd.prDraft', 'PR 草稿')}
        open={prDraftOpen}
        onCancel={() => setPrDraftOpen(false)}
        footer={[
          <Button key="close" onClick={() => setPrDraftOpen(false)}>{t('common.close')}</Button>,
          <Button key="share" icon={<ShareAltOutlined />} disabled={!prDraftQuery.data?.markdown} onClick={handleSharePrDraft}>
            {t('rd.sharePrDraft', '分享 PR 草稿')}
          </Button>,
          <Button key="download" icon={<DownloadOutlined />} disabled={!prDraftQuery.data?.markdown} onClick={handleDownloadPrDraft}>
            {t('rd.downloadPrDraft', '下载 PR 草稿')}
          </Button>,
          <Button
            key="publish"
            type="primary"
            danger
            icon={<RocketOutlined />}
            loading={publishPrDraftMutation.isPending}
            disabled={!prDraftQuery.data?.markdown || !prDraftIntegrationId}
            onClick={handlePublishPrDraft}
          >
            {t('rd.publishPrDraft', '确认推送')}
          </Button>,
        ]}
        width={900}
      >
        <Space direction="vertical" size={14} style={{ width: '100%' }}>
          <Alert
            type="info"
            showIcon
            message={t('rd.prDraftSafeModeTitle', '先预览草稿，确认后再推送')}
            description={t('rd.prDraftSafeModeDesc', '选择外部集成后会生成 GitHub/GitLab/Custom payload；点击确认推送才会真实调用远端接口。')}
          />
          <Select
            allowClear
            style={{ width: '100%' }}
            loading={integrationsQuery.isLoading}
            value={prDraftIntegrationId}
            onChange={setPrDraftIntegrationId}
            placeholder={t('rd.selectIntegrationOptional', '可选：选择外部集成以生成平台 payload')}
            options={enabledIntegrations.map((integration) => ({
              value: integration.id,
              label: `${integration.name} · ${integration.provider}`,
            }))}
          />
          {prDraftQuery.isLoading ? (
            <Spin />
          ) : prDraftQuery.data ? (
            <>
              <Card size="small" title={prDraftQuery.data.title}>
                <Markdown>{prDraftQuery.data.markdown}</Markdown>
              </Card>
              {prDraftQuery.data.providerPayloads.length > 0 ? (
                <Card size="small" title={t('rd.providerPayload', '平台 Payload')}>
                  <pre style={{ margin: 0, whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>
                    {JSON.stringify(prDraftQuery.data.providerPayloads, null, 2)}
                  </pre>
                </Card>
              ) : null}
            </>
          ) : (
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.prDraftEmpty', '暂无 PR 草稿')} />
          )}
        </Space>
      </Modal>
      <QuickOpenPalette
        open={!!quickOpenMode}
        mode={quickOpenMode ?? 'file'}
        repository={taskRepo}
        onClose={() => setQuickOpenMode(null)}
        onOpen={handleSelectWorkbenchFile}
      />
      <CommandPalette
        open={commandPaletteOpen}
        hasRepository={!!taskRepo}
        hasTask={!!selectedTask}
        hasPendingChanges={hasPendingChanges}
        onClose={() => setCommandPaletteOpen(false)}
        onSelectTab={(tab) => {
          if (['diff', 'tests', 'context', 'references', 'preview'].includes(tab)) {
            setActiveInspectorTab(tab);
          } else {
            setActiveWorkspaceTab(tab);
          }
        }}
        onQuickOpenFiles={() => setQuickOpenMode('file')}
        onQuickOpenSymbols={() => setQuickOpenMode('symbol')}
        onApplyAll={() => handleApply()}
        onRunTest={handleRunTest}
        onStartPreview={() => setActiveInspectorTab('preview')}
      />
    </div>
  );
}
