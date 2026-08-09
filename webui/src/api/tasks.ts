import { client } from './client';
import { getStoredAuthToken } from '@/store/auth';
import { createTaskEventStreamParser } from './taskEventStream';

export type TaskBucket = 'active' | 'waiting' | 'following' | 'history' | 'failed';

export interface TaskProgress {
  phaseCode?: string;
  phaseLabel?: string;
  activityCode?: string;
  activityText?: string;
  progressKind?: 'unknown' | 'stages' | 'items' | 'percent';
  current?: number;
  total?: number;
  unit?: string;
  confidence?: number;
  elapsedMs?: number;
  blockingReasonCode?: string;
  blockingReasonText?: string;
  providedInput?: unknown;
}

export interface TaskItem {
  id: string;
  shortCode: string;
  rootTaskId: string;
  parentTaskId?: string | null;
  title: string;
  summary?: string | null;
  capabilityKey: string;
  source: string;
  sourceLabel?: string | null;
  status: string;
  phase: string;
  stateVersion: number;
  progressPercent: number;
  progress?: TaskProgress | null;
  desiredState?: string | null;
  lastEvent?: string | null;
  errorCode?: string | null;
  errorMessage?: string | null;
  resultSummary?: string | null;
  resultArtifactRef?: string | null;
  sensitivityLabel: string;
  originSessionId?: string | null;
  originTurnId?: string | null;
  externalPlatform?: string | null;
  externalConversationId?: string | null;
  ownerUserId?: string | null;
  initiatorUserId?: string | null;
  assignedUserId?: string | null;
  lastProgressAt?: string | null;
  slaDueAt?: string | null;
  budget?: Record<string, unknown> | null;
  cost?: Record<string, unknown> | null;
  archived: boolean;
  createdAt: string;
  updatedAt: string;
  startedAt?: string | null;
  completedAt?: string | null;
  allowedActions?: string[];
}

export interface TaskEvent {
  id: number;
  eventId: string;
  taskId: string;
  rootTaskId: string;
  eventType: string;
  stateVersion: number;
  visibility: string;
  payload?: {
    phase?: string;
    status?: string;
    severity?: string;
    message?: string;
    metadata?: Record<string, unknown> | null;
    commandId?: string;
    commandType?: string;
    result?: unknown;
    error?: string | null;
  } | null;
  createdAt: string;
}

export interface TaskResource {
  id: string;
  resourceType: string;
  resourceId: string;
  relationType: string;
  metadata?: Record<string, unknown> | null;
  createdAt: string;
}

export interface TaskArtifact {
  id: string;
  artifactType: string;
  name: string;
  artifactRef: string;
  contentHash?: string | null;
  mimeType?: string | null;
  sizeBytes: number;
  sensitivityLabel: string;
  metadata?: Record<string, unknown> | null;
  createdAt: string;
}

export interface TaskArtifactContent {
  id: string;
  name: string;
  artifactRef: string;
  mimeType?: string | null;
  content: unknown;
}

export interface TaskCommand {
  id: string;
  actorUserId?: string | null;
  actorType: string;
  commandType: string;
  status: string;
  expectedStateVersion?: number | null;
  input?: unknown;
  result?: unknown;
  errorMessage?: string | null;
  attemptCount: number;
  createdAt: string;
  completedAt?: string | null;
}

export interface TaskAttempt {
  id: string;
  taskId: string;
  attemptNo: number;
  triggerType: string;
  triggerRef?: string | null;
  status: string;
  workerId?: string | null;
  errorCode?: string | null;
  errorMessage?: string | null;
  metadata?: Record<string, unknown> | null;
  startedAt?: string | null;
  completedAt?: string | null;
  createdAt: string;
}

export interface TaskSubscription {
  id: string;
  eventTypes: string[];
  destinationType: 'webui' | 'bot' | 'webhook';
  destinationRef?: string | null;
  policy?: Record<string, unknown> | null;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface WatchRule {
  id: string;
  name: string;
  scopeType: 'own' | 'task' | 'capability';
  scopeRef?: string | null;
  condition: Record<string, unknown>;
  action: Record<string, unknown>;
  quietHours?: Record<string, unknown> | null;
  maxActionsPerDay: number;
  requiresConfirmation: boolean;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface WatchRulePendingAction {
  runId: number;
  taskId: string;
  shortCode?: string | null;
  taskTitle: string;
  taskStatus: string;
  ruleName: string;
  action: Record<string, unknown>;
  detail?: Record<string, unknown> | null;
  createdAt: string;
}

export interface ExternalIdentity {
  id: string;
  platform: string;
  externalUserId: string;
  channelId?: string | null;
  externalConversationId?: string | null;
  displayName?: string | null;
  status: string;
  verifiedAt: string;
  lastSeenAt?: string | null;
}

export interface NotificationDelivery {
  id: string;
  taskId: string;
  shortCode?: string | null;
  title: string;
  platform: string;
  channelId?: string | null;
  status: string;
  attemptCount: number;
  maxAttempts: number;
  providerMessageId?: string | null;
  lastError?: string | null;
  payload?: Record<string, unknown> | null;
  sentAt?: string | null;
  createdAt: string;
  updatedAt: string;
  allowedActions: string[];
}

export interface TaskSummary {
  running: number;
  waiting: number;
  failed: number;
  byStatus: Array<{ status: string; count: number }>;
  scope: 'own' | 'tenant';
}

export interface TaskListParams {
  scope?: 'own' | 'tenant';
  status?: string;
  bucket?: TaskBucket;
  capabilityKey?: string;
  cursor?: string;
  limit?: number;
  includeArchived?: boolean;
  includeChildren?: boolean;
}

const TIMEZONE_SUFFIX = /(?:Z|[+-]\d{2}:?\d{2})$/i;
const DATABASE_TIMESTAMP = /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?$/;

/** Database DATETIME values are emitted in UTC but do not include an offset. */
export function parseTaskTimestamp(value?: string | null): number {
  if (!value) return Number.NaN;
  const normalized = value.trim();
  if (DATABASE_TIMESTAMP.test(normalized) && !TIMEZONE_SUFFIX.test(normalized)) {
    return Date.parse(`${normalized.replace(' ', 'T')}Z`);
  }
  return Date.parse(normalized);
}

const ref = (value: string) => encodeURIComponent(value);

export const tasksApi = {
  summary: (scope: 'own' | 'tenant' = 'own') =>
    client.get<TaskSummary>('/tasks/summary', { params: { scope } }).then((response) => response.data),
  list: (params: TaskListParams = {}) =>
    client
      .get<{ items: TaskItem[]; nextCursor?: string | null }>('/tasks', { params })
      .then((response) => response.data),
  detail: (taskRef: string) =>
    client.get<TaskItem>(`/tasks/${ref(taskRef)}`).then((response) => response.data),
  events: (taskRef: string, params?: { afterId?: number; limit?: number }) =>
    client
      .get<{ items: TaskEvent[] }>(`/tasks/${ref(taskRef)}/events`, { params })
      .then((response) => response.data),
  resources: (taskRef: string) =>
    client
      .get<{ items: TaskResource[] }>(`/tasks/${ref(taskRef)}/resources`)
      .then((response) => response.data),
  artifacts: (taskRef: string) =>
    client
      .get<{ items: TaskArtifact[] }>(`/tasks/${ref(taskRef)}/artifacts`)
      .then((response) => response.data),
  artifactContent: (taskRef: string, artifactId: string) =>
    client
      .get<TaskArtifactContent>(`/tasks/${ref(taskRef)}/artifacts/${ref(artifactId)}`)
      .then((response) => response.data),
  attempts: (taskRef: string) =>
    client
      .get<{ items: TaskAttempt[] }>(`/tasks/${ref(taskRef)}/attempts`)
      .then((response) => response.data),
  commands: (taskRef: string) =>
    client
      .get<{ items: TaskCommand[] }>(`/tasks/${ref(taskRef)}/commands`)
      .then((response) => response.data),
  command: (
    taskRef: string,
    commandType: string,
    options?: { expectedStateVersion?: number; idempotencyKey?: string; input?: unknown },
  ) =>
    client
      .post<{ accepted: boolean; commandId: string; status: string; reused: boolean }>(
        `/tasks/${ref(taskRef)}/commands`,
        { commandType, ...options },
      )
      .then((response) => response.data),
  subscriptions: (taskRef: string) =>
    client
      .get<{ items: TaskSubscription[] }>(`/tasks/${ref(taskRef)}/subscriptions`)
      .then((response) => response.data),
  subscribe: (
    taskRef: string,
    data: {
      eventTypes: string[];
      destinationType: 'webui' | 'bot' | 'webhook';
      destinationRef?: string;
      policy?: Record<string, unknown>;
    },
  ) =>
    client
      .post<{ id: string; taskId: string; enabled: boolean }>(
        `/tasks/${ref(taskRef)}/subscriptions`,
        data,
      )
      .then((response) => response.data),
  unsubscribe: (taskRef: string, subscriptionId: string) =>
    client
      .delete(`/tasks/${ref(taskRef)}/subscriptions/${ref(subscriptionId)}`)
      .then((response) => response.data),
  share: (taskRef: string, userId: string, permission: 'read' | 'control') =>
    client
      .post(`/tasks/${ref(taskRef)}/share`, { userId, permission })
      .then((response) => response.data),
  presence: (data: {
    clientId: string;
    currentPath?: string;
    mobileFollowEnabled?: boolean;
    ttlSeconds?: number;
  }) => client.post('/tasks/presence', data).then((response) => response.data),
  presenceSettings: () =>
    client
      .get<{ mobileFollowEnabled: boolean }>('/tasks/presence')
      .then((response) => response.data),
  watchRules: () =>
    client.get<{ items: WatchRule[] }>('/tasks/watch-rules').then((response) => response.data),
  createWatchRule: (data: {
    name: string;
    scopeType?: 'own' | 'task' | 'capability';
    scopeRef?: string;
    condition: Record<string, unknown>;
    action: Record<string, unknown>;
    quietHours?: Record<string, unknown>;
    maxActionsPerDay?: number;
    requiresConfirmation?: boolean;
    enabled?: boolean;
  }) => client.post<{ id: string; enabled: boolean }>('/tasks/watch-rules', data).then((response) => response.data),
  deleteWatchRule: (ruleId: string) =>
    client.delete(`/tasks/watch-rules/${ref(ruleId)}`).then((response) => response.data),
  pendingWatchRuleActions: () =>
    client
      .get<{ items: WatchRulePendingAction[] }>('/tasks/watch-rules/pending')
      .then((response) => response.data),
  decideWatchRuleAction: (runId: number, approve: boolean) =>
    client
      .post<{ ok: boolean; status: string; outcome?: Record<string, unknown> }>(
        `/tasks/watch-rules/runs/${runId}/decision`,
        { approve },
      )
      .then((response) => response.data),
  deliveries: (params?: {
    scope?: 'own' | 'tenant';
    status?: string;
    page?: number;
    perPage?: number;
  }) =>
    client
      .get<{
        items: NotificationDelivery[];
        scope: string;
        total: number;
        page: number;
        perPage: number;
      }>('/tasks/deliveries', { params })
      .then((response) => response.data),
  replayDelivery: (deliveryId: string) =>
    client
      .post<{ ok: boolean; deliveryId: string; status: string }>(
        `/tasks/deliveries/${ref(deliveryId)}/replay`,
      )
      .then((response) => response.data),
  identities: () =>
    client.get<{ items: ExternalIdentity[] }>('/bot-identities').then((response) => response.data),
  createPairingCode: (platform?: string) =>
    client
      .post<{ code: string; expiresInSeconds: number }>('/bot-identities/pairing-codes', {
        platform,
      })
      .then((response) => response.data),
  revokeIdentity: (identityId: string) =>
    client.delete(`/bot-identities/${ref(identityId)}`).then((response) => response.data),
};

export async function streamTaskEvents(options: {
  afterEventId: number;
  scope?: 'own' | 'tenant';
  signal: AbortSignal;
  onEvent: (event: TaskEvent) => void;
  onWarning?: (message: string) => void;
}): Promise<void> {
  const token = getStoredAuthToken();
  if (!token) throw new Error('Authentication required');
  const tenantId = localStorage.getItem('tenant_id');
  const params = new URLSearchParams({
    afterEventId: String(Math.max(0, options.afterEventId)),
    scope: options.scope ?? 'own',
  });
  const response = await fetch(`/api/v1/tasks/stream?${params.toString()}`, {
    headers: {
      Accept: 'text/event-stream',
      Authorization: `Bearer ${token}`,
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
    signal: options.signal,
  });
  if (!response.ok || !response.body) {
    throw new Error(`Task event stream failed (${response.status})`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const parser = createTaskEventStreamParser({
    onEvent: options.onEvent,
    onWarning: options.onWarning,
  });
  while (!options.signal.aborted) {
    const { done, value } = await reader.read();
    if (done) break;
    parser.push(decoder.decode(value, { stream: true }));
  }
  parser.push(decoder.decode());
  parser.finish();
}
