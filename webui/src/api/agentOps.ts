import { client } from './client';

export interface AgentOpsCapabilityContract {
  key: string;
  displayName: string;
  menuKey: string;
  executionMode: string;
  supportsBot: boolean;
  supportsWatchdog: boolean;
  requiredPermissions: string[];
  rollout: string;
  description: string;
}

export interface AgentOpsBotPlatformContract {
  key: string;
  displayName: string;
  inboundModes: string[];
  supportsLocalInbound: boolean;
  supportsOutbound: boolean;
  requiresPublicCallback: boolean;
  description: string;
}

export interface AgentOpsRuntimeContract {
  key: string;
  displayName: string;
  isolationMode: string;
  supportsProcessGroupCancel: boolean;
  supportsArtifacts: boolean;
  defaultEnabled: boolean;
  description: string;
}

export interface AgentOpsWatchDogActionContract {
  key: string;
  displayName: string;
  permission: string;
  auditAction: string;
}

export interface AgentOpsCapabilityContractsResponse {
  items: AgentOpsCapabilityContract[];
  botPlatforms: AgentOpsBotPlatformContract[];
  runtimes: AgentOpsRuntimeContract[];
  watchdogActions: AgentOpsWatchDogActionContract[];
}

export interface AgentOpsTask {
  id: string;
  tenantId: string;
  source: string;
  sourceRef?: string | null;
  sourceLabel?: string | null;
  capabilityKey: string;
  agentId?: string | null;
  agentName?: string | null;
  status: string;
  phase: string;
  progressPercent: number;
  title: string;
  summary?: string | null;
  ownerUserId?: string | null;
  externalPlatform?: string | null;
  externalChannelId?: string | null;
  externalConversationId?: string | null;
  linkedResourceType?: string | null;
  linkedResourceId?: string | null;
  inputJson?: Record<string, unknown> | null;
  outputJson?: Record<string, unknown> | null;
  runtimeSession?: {
    id: string;
    status: string;
    workspaceRoot: string;
    isolationMode: string;
    cancelRequested: boolean;
    heartbeatAt?: string | null;
    currentCommand?: string | null;
    currentProcessStatus?: string | null;
  } | null;
  queue: {
    status: string;
    availableAt?: string | null;
    claimedBy?: string | null;
    claimedAt?: string | null;
    leaseExpiresAt?: string | null;
    attemptCount: number;
    maxAttempts: number;
    idempotencyKey?: string | null;
    priority: number;
    lastError?: string | null;
    finishedAt?: string | null;
    deadReason?: string | null;
  };
  errorMessage?: string | null;
  lastEvent?: string | null;
  lastHeartbeatAt?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AgentOpsTaskEvent {
  id: string;
  taskId: string;
  eventType: string;
  phase?: string | null;
  status?: string | null;
  severity: string;
  message: string;
  metadataJson?: Record<string, unknown> | null;
  createdAt: string;
}

export interface AgentTraceEvent extends AgentOpsTaskEvent {
  artifactId?: string | null;
  runtimeSessionId?: string | null;
  runtimeProcessId?: string | null;
  tokenInput?: number | null;
  tokenOutput?: number | null;
  costUsd?: number | null;
  durationMs?: number | null;
}

export interface AgentRuntimeProcess {
  id: string;
  runtimeSessionId: string;
  agentTaskId?: string | null;
  command: string;
  cwd: string;
  status: string;
  pid?: number | null;
  processGroupId?: number | null;
  exitCode?: number | null;
  stdoutPreview?: string | null;
  stderrPreview?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AgentRuntimeArtifact {
  id: string;
  runtimeSessionId: string;
  agentTaskId?: string | null;
  artifactType: string;
  path?: string | null;
  contentText?: string | null;
  contentHash?: string | null;
  sizeBytes: number;
  createdAt: string;
}

export interface AgentRuntimeArtifactDetail {
  id: string;
  tenantId: string;
  runtimeSessionId: string;
  agentTaskId?: string | null;
  artifactType: string;
  path?: string | null;
  contentText?: string | null;
  contentHash?: string | null;
  sizeBytes: number;
  createdAt: string;
  content?: string | null;
  contentTruncated: boolean;
  readSource: string;
}

export interface AgentOpsSummary {
  running: number;
  stale: number;
  byStatus: Array<{ status: string; count: number }>;
  capabilities: AgentOpsCapabilityContract[];
}

export interface AgentOpsAgentHealth {
  capabilityKey: string;
  displayName: string;
  total: number;
  active: number;
  failed24h: number;
  lastUpdatedAt?: string | null;
}

export interface WatchDogAskResponse {
  answer?: string;
  async?: boolean;
  taskId?: string;
  status?: string;
}

export const agentOpsApi = {
  summary: () => client.get<AgentOpsSummary>('/agent-ops/summary').then((r) => r.data),

  agents: () =>
    client.get<{ items: AgentOpsAgentHealth[] }>('/agent-ops/agents').then((r) => r.data),

  tasks: (params?: {
    status?: string;
    attention_only?: boolean;
    capability_key?: string;
    source?: string;
    external_conversation_id?: string;
    linked_resource_type?: string;
    linked_resource_id?: string;
    page?: number;
    per_page?: number;
  }) => client.get<{ items: AgentOpsTask[]; total: number }>('/agent-ops/tasks', { params }).then((r) => r.data),

  queue: (params?: {
    queueStatus?: string;
    capabilityKey?: string;
    workerId?: string;
    deadOnly?: boolean;
    staleOnly?: boolean;
    leaseTimeoutSecs?: number;
    page?: number;
    per_page?: number;
  }) => client.get<{ items: AgentOpsTask[]; total: number }>('/agent-ops/queue', { params }).then((r) => r.data),

  task: (id: string) => client.get<AgentOpsTask>(`/agent-ops/tasks/${id}`).then((r) => r.data),

  taskEvents: (id: string, params?: { page?: number; per_page?: number }) =>
    client.get<{ items: AgentOpsTaskEvent[]; total: number }>(`/agent-ops/tasks/${id}/events`, { params }).then((r) => r.data),

  taskTrace: (id: string, params?: { page?: number; per_page?: number }) =>
    client.get<{ items: AgentTraceEvent[]; total: number }>(`/agent-ops/tasks/${id}/trace`, { params }).then((r) => r.data),

  cancelTask: (id: string) => client.post<{ ok: boolean; status: string }>(`/agent-ops/tasks/${id}/cancel`).then((r) => r.data),

  retryTask: (id: string) => client.post<{ ok: boolean; status: string; message: string }>(`/agent-ops/tasks/${id}/retry`).then((r) => r.data),

  recoverQueue: (data?: { leaseTimeoutSecs?: number }) =>
    client.post<{ ok: boolean; timeoutSecs: number; dead: number; recovered: number }>('/agent-ops/queue/recover', data ?? {}).then((r) => r.data),

  recoverRuntime: (data?: { timeout_secs?: number }) =>
    client.post<{ recovered: number; timeoutSecs: number }>('/agent-runtime/sessions/recover', data ?? {}).then((r) => r.data),

  runtimeProcesses: (sessionId: string, params?: { page?: number; per_page?: number }) =>
    client.get<{ items: AgentRuntimeProcess[]; total: number }>(`/agent-runtime/sessions/${sessionId}/processes`, { params }).then((r) => r.data),

  runtimeArtifacts: (sessionId: string, params?: { page?: number; per_page?: number }) =>
    client.get<{ items: AgentRuntimeArtifact[]; total: number }>(`/agent-runtime/sessions/${sessionId}/artifacts`, { params }).then((r) => r.data),

  runtimeArtifact: (sessionId: string, artifactId: string) =>
    client.get<AgentRuntimeArtifactDetail>(`/agent-runtime/sessions/${sessionId}/artifacts/${artifactId}`).then((r) => r.data),

  ask: (data: {
    question: string;
    scope?: string;
    external_platform?: string;
    external_channel_id?: string;
    external_conversation_id?: string;
    asyncMode?: boolean;
  }) => client.post<WatchDogAskResponse>('/agent-ops/watchdog/ask', data).then((r) => r.data),

  capabilities: () =>
    client.get<AgentOpsCapabilityContractsResponse>('/agent-ops/capabilities').then((r) => r.data),
};
