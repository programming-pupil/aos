import { client } from './client';
import type {
  RdRepository,
  RdRepositoryListResponse,
  RdFileNode,
  RdFileContentResponse,
  RdRepositoryFileSuggestion,
  RdRepositoryWorktreeStatus,
  RdRepositorySearchHit,
  RdRepositorySymbol,
  RdRepositoryImport,
  RdCodeIntelAction,
  RdCodeIntelQueryResponse,
  RdCodeIntelStatusResponse,
  RdPreviewEvent,
  RdPreviewLogsResponse,
  RdPreviewSession,
  RdIntentRouteResponse,
  RdTask,
  RdTaskListResponse,
  RdTaskEvent,
  RdTaskEventListResponse,
  RdFileChange,
  RdApplyHunksResponse,
  RdTaskRollbackResponse,
  RdTestRun,
  RdTaskWorkbenchResponse,
  RdQualitySummary,
  RdSpec,
  RdSpecEvent,
  RdAgentProfile,
  RdAgentMarketInstallResponse,
  RdAgentMarketSearchResponse,
  RdAgentWorkflow,
  RdSteeringRule,
  RdIntegration,
  RdIntegrationTestResult,
  RdPrDraft,
  RdPrDraftPublishResult,
  RdTaskMode,
  RdBaselinePolicy,
} from '@/types';

// ---- AOS Code Studio / R&D ----
export const rdApi = {
  listRepositories: () =>
    client.get<RdRepositoryListResponse>('/rd/repositories').then((r) => r.data),

  createRepository: (data: {
    name: string;
    url: string;
    branch?: string;
    gitlab_token?: string;
    description?: string;
    default_test_command?: string;
    default_build_command?: string;
    auto_sync_enabled?: boolean;
    auto_sync_interval_minutes?: number;
  }) => client.post<RdRepository>('/rd/repositories', data).then((r) => r.data),

  updateRepository: (id: string, data: {
    name?: string;
    url?: string;
    branch?: string;
    gitlab_token?: string;
    description?: string;
    default_test_command?: string;
    default_build_command?: string;
    auto_sync_enabled?: boolean;
    auto_sync_interval_minutes?: number;
  }) => client.patch<RdRepository>(`/rd/repositories/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteRepository: (id: string) =>
    client.delete<{ deleted: boolean }>(`/rd/repositories/${encodeURIComponent(id)}`).then((r) => r.data),

  syncRepository: (id: string) =>
    client.post<{
      accepted: boolean;
      status: string;
      synced?: boolean;
      clonePath?: string;
      indexedFileCount?: number;
      symbolCount?: number;
      importCount?: number;
      detection?: {
        primaryLanguage?: string | null;
        languages?: Array<{ language: string; fileCount: number }>;
        stack?: string[];
        packageManager?: string | null;
        detectedTestCommand?: string | null;
        detectedBuildCommand?: string | null;
      };
    }>(
      `/rd/repositories/${encodeURIComponent(id)}/sync`,
      {},
    ).then((r) => r.data),

  repositoryTree: (id: string) =>
    client.get<RdFileNode[]>(`/rd/repositories/${encodeURIComponent(id)}/tree`).then((r) => r.data),

  repositoryFile: (id: string, path: string) =>
    client.get<RdFileContentResponse>(`/rd/repositories/${encodeURIComponent(id)}/file`, {
      params: { path },
    }).then((r) => r.data),

  repositorySearch: (id: string, params: { q: string; limit?: number }) =>
    client.get<RdRepositorySearchHit[]>(`/rd/repositories/${encodeURIComponent(id)}/search`, {
      params,
    }).then((r) => r.data),

  repositoryFileSuggestions: (id: string, params?: { q?: string; limit?: number }) =>
    client.get<RdRepositoryFileSuggestion[]>(`/rd/repositories/${encodeURIComponent(id)}/file-suggestions`, {
      params,
    }).then((r) => r.data),

  repositoryWorktreeStatus: (id: string) =>
    client.get<RdRepositoryWorktreeStatus>(`/rd/repositories/${encodeURIComponent(id)}/worktree-status`).then((r) => r.data),

  repositorySymbols: (id: string, params?: { q?: string; limit?: number }) =>
    client.get<RdRepositorySymbol[]>(`/rd/repositories/${encodeURIComponent(id)}/symbols`, {
      params,
    }).then((r) => r.data),

  repositoryImports: (id: string, params?: { q?: string; limit?: number }) =>
    client.get<RdRepositoryImport[]>(`/rd/repositories/${encodeURIComponent(id)}/imports`, {
      params,
    }).then((r) => r.data),

  codeIntelStatus: (id: string) =>
    client.get<RdCodeIntelStatusResponse>(`/rd/repositories/${encodeURIComponent(id)}/code-intel/status`).then((r) => r.data),

  codeIntelQuery: (id: string, data: {
    action: RdCodeIntelAction;
    path?: string;
    line?: number;
    character?: number;
    query?: string;
  }) =>
    client.post<RdCodeIntelQueryResponse>(`/rd/repositories/${encodeURIComponent(id)}/code-intel/query`, data).then((r) => r.data),

  codeIntelRestart: (id: string) =>
    client.post<RdCodeIntelStatusResponse>(`/rd/repositories/${encodeURIComponent(id)}/code-intel/restart`, {}).then((r) => r.data),

  createPreviewSession: (id: string, data: {
    command: string;
    port?: number;
    path?: string;
    taskId?: string;
  }) =>
    client.post<RdPreviewSession>(`/rd/repositories/${encodeURIComponent(id)}/preview-sessions`, data).then((r) => r.data),

  getPreviewSession: (id: string) =>
    client.get<RdPreviewSession>(`/rd/preview-sessions/${encodeURIComponent(id)}`).then((r) => r.data),

  authorizePreviewSession: (id: string) =>
    client.post<{ url: string; expiresInSeconds: number }>(`/rd/preview-sessions/${encodeURIComponent(id)}/authorize`, {}).then((r) => r.data),

  stopPreviewSession: (id: string) =>
    client.post<RdPreviewSession>(`/rd/preview-sessions/${encodeURIComponent(id)}/stop`, {}).then((r) => r.data),

  previewSessionLogs: (id: string) =>
    client.get<RdPreviewLogsResponse>(`/rd/preview-sessions/${encodeURIComponent(id)}/logs`).then((r) => r.data),

  previewScreenshot: (id: string) =>
    client.post<RdPreviewEvent>(`/rd/preview-sessions/${encodeURIComponent(id)}/screenshot`, {}).then((r) => r.data),

  recordPreviewConsoleEvent: (id: string, data: {
    eventType?: string;
    severity?: string;
    message: string;
    metadataJson?: Record<string, unknown>;
  }) =>
    client.post<RdPreviewEvent>(`/rd/preview-sessions/${encodeURIComponent(id)}/console-event`, data).then((r) => r.data),

  repositoryBranches: (id: string) =>
    client.get<{ branches: string[] }>(`/rd/repositories/${encodeURIComponent(id)}/branches`).then((r) => r.data),

  quality: (params?: { days?: number; repositoryId?: string }) =>
    client.get<RdQualitySummary>('/rd/quality', {
      params: {
        days: params?.days,
        repository_id: params?.repositoryId,
      },
    }).then((r) => r.data),

  routeIntent: (data: { prompt: string; model?: string }) =>
    client.post<RdIntentRouteResponse>('/rd/intent-route', data).then((r) => r.data),

  listTasks: (params?: {
    status?: string;
    repositoryId?: string;
    mode?: RdTaskMode | string;
    page?: number;
    perPage?: number;
  }) =>
    client.get<RdTaskListResponse>('/rd/tasks', {
      params: {
        status: params?.status,
        repository_id: params?.repositoryId,
        mode: params?.mode,
        page: params?.page,
        per_page: params?.perPage,
      },
    }).then((r) => r.data),

  createTask: (data: {
    repositoryId?: string;
    specId?: string;
    agentProfileId?: string;
    workflowId?: string;
    parentTaskId?: string;
    baselinePolicy?: RdBaselinePolicy;
    mode?: RdTaskMode;
    contextProfile?: string;
    contextDepth?: string;
    shouldDeepScan?: boolean;
    title?: string;
    prompt: string;
    model?: string;
  }) => client.post<RdTask>('/rd/tasks', data).then((r) => r.data),

  getTask: (id: string) =>
    client.get<RdTask>(`/rd/tasks/${encodeURIComponent(id)}`).then((r) => r.data),

  taskWorkbench: (id: string) =>
    client.get<RdTaskWorkbenchResponse>(`/rd/tasks/${encodeURIComponent(id)}/workbench`).then((r) => r.data),

  taskEventsPage: (id: string, params?: { cursorBefore?: number; perPage?: number }) =>
    client.get<RdTaskEventListResponse>(`/rd/tasks/${encodeURIComponent(id)}/events`, {
      params: {
        cursor_before: params?.cursorBefore,
        per_page: params?.perPage,
      },
    }).then((r) => r.data),

  taskTokenDiagnostics: (id: string) =>
    client.get<RdTaskEventListResponse>(`/rd/tasks/${encodeURIComponent(id)}/token-diagnostics`)
      .then((r) => r.data),

  taskEvents: (id: string) =>
    rdApi.taskEventsPage(id).then((data) => data.events),

  taskChanges: (id: string) =>
    client.get<RdFileChange[]>(`/rd/tasks/${encodeURIComponent(id)}/changes`).then((r) => r.data),

  taskTests: (id: string) =>
    client.get<RdTestRun[]>(`/rd/tasks/${encodeURIComponent(id)}/tests`).then((r) => r.data),

  applyChanges: (id: string, changeIds?: string[]) =>
    client.post<{ applied: number; skipped: number }>(`/rd/tasks/${encodeURIComponent(id)}/apply`, {
      change_ids: changeIds,
    }).then((r) => r.data),

  rollbackChanges: (id: string, changeIds?: string[]) =>
    client.post<RdTaskRollbackResponse>(`/rd/tasks/${encodeURIComponent(id)}/rollback`, {
      change_ids: changeIds,
    }).then((r) => r.data),

  applyHunks: (id: string, changeId: string, hunkIndexes: number[]) =>
    client.post<RdApplyHunksResponse>(`/rd/tasks/${encodeURIComponent(id)}/apply-hunks`, {
      changeId,
      hunkIndexes,
    }).then((r) => r.data),

  runTest: (id: string, command?: string) =>
    client.post<RdTestRun>(`/rd/tasks/${encodeURIComponent(id)}/test`, { command }).then((r) => r.data),

  cancelTask: (id: string) =>
    client.post<{ cancelled: boolean }>(`/rd/tasks/${encodeURIComponent(id)}/cancel`, {}).then((r) => r.data),

  retryTask: (id: string) =>
    client.post<RdTask>(`/rd/tasks/${encodeURIComponent(id)}/retry`, {}).then((r) => r.data),

  listSpecs: () =>
    client.get<RdSpec[]>('/rd/specs').then((r) => r.data),

  createSpec: (data: { repositoryId?: string; repositoryIds?: string[]; title?: string; prompt: string; model?: string; mode?: string }) =>
    client.post<RdSpec>('/rd/specs', data).then((r) => r.data),

  getSpec: (id: string) =>
    client.get<RdSpec>(`/rd/specs/${encodeURIComponent(id)}`).then((r) => r.data),

  updateSpec: (id: string, data: {
    title?: string;
    requirementsMd?: string;
    designMd?: string;
    tasksMd?: string;
    acceptanceMd?: string;
    taskItems?: RdSpec['taskItems'];
  }) => client.patch<RdSpec>(`/rd/specs/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteSpec: (id: string) =>
    client.delete<{ deleted: boolean }>(`/rd/specs/${encodeURIComponent(id)}`).then((r) => r.data),

  specEvents: (id: string) =>
    client.get<RdSpecEvent[]>(`/rd/specs/${encodeURIComponent(id)}/events`).then((r) => r.data),

  generateSpec: (id: string) =>
    client.post<RdSpec>(`/rd/specs/${encodeURIComponent(id)}/generate-spec`, {}).then((r) => r.data),

  approveSpec: (id: string) =>
    client.post<RdSpec>(`/rd/specs/${encodeURIComponent(id)}/approve-spec`, {}).then((r) => r.data),

  generateDesign: (id: string) =>
    client.post<RdSpec>(`/rd/specs/${encodeURIComponent(id)}/generate-design`, {}).then((r) => r.data),

  approveDesign: (id: string) =>
    client.post<RdSpec>(`/rd/specs/${encodeURIComponent(id)}/approve-design`, {}).then((r) => r.data),

  generateTasks: (id: string) =>
    client.post<RdSpec>(`/rd/specs/${encodeURIComponent(id)}/generate-tasks`, {}).then((r) => r.data),

  approveTasks: (id: string) =>
    client.post<RdSpec>(`/rd/specs/${encodeURIComponent(id)}/approve-tasks`, {}).then((r) => r.data),

  implementSpecTask: (id: string, data?: {
    taskItemId?: string;
    model?: string;
    agentProfileId?: string;
    workflowId?: string;
  }) => client.post<RdTask>(`/rd/specs/${encodeURIComponent(id)}/implement-task`, data ?? {}).then((r) => r.data),

  implementAllSpecTasks: (id: string, data?: {
    model?: string;
    agentProfileId?: string;
    workflowId?: string;
  }) => client.post<RdTask[]>(`/rd/specs/${encodeURIComponent(id)}/implement-all`, data ?? {}).then((r) => r.data),

  finalReportSpec: (id: string) =>
    client.post<RdSpec>(`/rd/specs/${encodeURIComponent(id)}/final-report`, {}).then((r) => r.data),

  createTaskFromSpec: (id: string, data?: { model?: string }) =>
    client.post<RdTask>(`/rd/specs/${encodeURIComponent(id)}/create-task`, data ?? {}).then((r) => r.data),

  listAgentProfiles: () =>
    client.get<RdAgentProfile[]>('/rd/agent-profiles').then((r) => r.data),

  createAgentProfile: (data: {
    name: string;
    rolePrompt: string;
    allowedTools?: string | Record<string, unknown> | unknown[] | null;
    defaultModel?: string;
    enabled?: boolean;
  }) => client.post<RdAgentProfile>('/rd/agent-profiles', data).then((r) => r.data),

  updateAgentProfile: (id: string, data: {
    name: string;
    rolePrompt: string;
    allowedTools?: string | Record<string, unknown> | unknown[] | null;
    defaultModel?: string;
    enabled?: boolean;
  }) => client.patch<RdAgentProfile>(`/rd/agent-profiles/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteAgentProfile: (id: string) =>
    client.delete<{ deleted: boolean }>(`/rd/agent-profiles/${encodeURIComponent(id)}`).then((r) => r.data),

  searchAgentMarket: (params?: { q?: string; itemType?: string }) =>
    client.get<RdAgentMarketSearchResponse>('/rd/agent-market/search', {
      params: {
        q: params?.q,
        item_type: params?.itemType,
      },
    }).then((r) => r.data),

  installAgentMarketItem: (id: string, data?: { defaultModel?: string; enabled?: boolean }) =>
    client.post<RdAgentMarketInstallResponse>(`/rd/agent-market/${encodeURIComponent(id)}/install`, data ?? {}).then((r) => r.data),

  listAgentWorkflows: () =>
    client.get<RdAgentWorkflow[]>('/rd/agent-workflows').then((r) => r.data),

  createAgentWorkflow: (data: {
    name: string;
    description?: string;
    definitionJson: Record<string, unknown>;
    enabled?: boolean;
  }) => client.post<RdAgentWorkflow>('/rd/agent-workflows', data).then((r) => r.data),

  updateAgentWorkflow: (id: string, data: {
    name: string;
    description?: string;
    definitionJson: Record<string, unknown>;
    enabled?: boolean;
  }) => client.patch<RdAgentWorkflow>(`/rd/agent-workflows/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteAgentWorkflow: (id: string) =>
    client.delete<{ deleted: boolean }>(`/rd/agent-workflows/${encodeURIComponent(id)}`).then((r) => r.data),

  listSteeringRules: () =>
    client.get<RdSteeringRule[]>('/rd/steering-rules').then((r) => r.data),

  createSteeringRule: (data: {
    repositoryId?: string;
    repositoryIds?: string[];
    name: string;
    description?: string;
    contentMd: string;
    enabled?: boolean;
  }) => client.post<RdSteeringRule>('/rd/steering-rules', data).then((r) => r.data),

  updateSteeringRule: (id: string, data: {
    repositoryId?: string;
    repositoryIds?: string[];
    name: string;
    description?: string;
    contentMd: string;
    enabled?: boolean;
  }) => client.patch<RdSteeringRule>(`/rd/steering-rules/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteSteeringRule: (id: string) =>
    client.delete<{ deleted: boolean }>(`/rd/steering-rules/${encodeURIComponent(id)}`).then((r) => r.data),

  listIntegrations: () =>
    client.get<RdIntegration[]>('/rd/integrations').then((r) => r.data),

  createIntegration: (data: {
    provider: string;
    name: string;
    configJson?: Record<string, unknown> | null;
    enabled?: boolean;
  }) => client.post<RdIntegration>('/rd/integrations', data).then((r) => r.data),

  updateIntegration: (id: string, data: {
    provider?: string;
    name?: string;
    configJson?: Record<string, unknown> | null;
    enabled?: boolean;
  }) => client.patch<RdIntegration>(`/rd/integrations/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteIntegration: (id: string) =>
    client.delete<{ deleted: boolean }>(`/rd/integrations/${encodeURIComponent(id)}`).then((r) => r.data),

  testIntegration: (id: string) =>
    client.post<RdIntegrationTestResult>(`/rd/integrations/${encodeURIComponent(id)}/test`, {}).then((r) => r.data),

  taskPrDraft: (id: string, params?: { integrationId?: string }) =>
    client.get<RdPrDraft>(`/rd/tasks/${encodeURIComponent(id)}/pr-draft`, {
      params: {
        integration_id: params?.integrationId,
      },
    }).then((r) => r.data),

  publishPrDraft: (id: string, data: { integrationId: string }) =>
    client.post<RdPrDraftPublishResult>(`/rd/tasks/${encodeURIComponent(id)}/pr-draft/publish`, {
      integrationId: data.integrationId,
    }).then((r) => r.data),
};
