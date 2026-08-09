import { client, fastClient } from './client';
import type {
  AgentSession,
  AgentSessionHistory,
  AgentSessionInfo,
  AgentToolCall,
  AgentTurnResult,
  AgentUsage,
  ChatAdversarialRun,
  ChatAdversarialStreamEvent,
} from '@/types';
import type {
  PmBudgetProfileActivateResponse,
  PmBudgetProfilesResponse,
  PmFailureTaxonomyResponse,
  PmKnowledgeCoverageWarningsResponse,
  PmProviderHealthResponse,
  PmQualityGateSummaryResponse,
  PmResearchTaskControlResponse,
  PmResearchTaskStartResponse,
  PmResearchTaskStatusResponse,
  PmRouteLearningFeaturesResponse,
  PmRuntimeInsightsResponse,
  PmSloSummaryResponse,
  PmStrategyLeaderboardResponse,
  PmStrategyRecordRequest,
  PmSubtaskAttemptRow,
  PmSubtaskRuntimeRow,
  PmTaskDocumentInput,
  PmTaskImageInput,
} from './pm';
import {
  createSuperAssistantEventState,
  reduceSuperAssistantEvent,
  type SuperAssistantEventEffect,
  type SuperAssistantEventState,
} from './superAssistantEventReducer';

export { chatApi, streamChat } from './chat';

export interface ChatTurnOptions {
  searchMode?: 'on' | 'off';
  searchEnabled?: boolean;
  fileContext?: {
    mode: 'none' | 'selected' | 'all_attached' | 'workspace';
    fileIds?: string[];
    strictGrounding?: boolean;
  };
  memoryMode?: 'auto' | 'off' | 'pinned_only';
}

export interface ChatCapabilityResponse {
  reasoning: {
    defaultBudget: string;
    userSelectable: boolean;
    supportsReasoningEffort: boolean;
    message: string;
  };
  model: {
    name: string;
    contextWindowTokens: number;
    maxOutputTokens: number;
    source: 'manual_override' | 'built_in_registry' | 'conservative_fallback' | string;
    conservativeFallback: boolean;
  };
  search: {
    enabled: boolean;
    defaultMode?: 'auto' | 'on' | 'off' | string;
    currentProvider?: string | null;
    missingReason?: string | null;
    builtin?: boolean;
    native?: boolean;
    mcp?: boolean;
    configuredProviders?: number;
    ragLocal?: boolean;
    providers: Array<{
      provider: string;
      configured: boolean;
      source: string;
      detail: string;
    }>;
  };
  fileContext: {
    enabled: boolean;
    strictGrounding: boolean;
    supportedMediaTypes: string[];
  };
  streaming?: {
    tokenDelta: boolean;
    fallbackTypewriter: boolean;
  };
  fileRag?: {
    enabled: boolean;
    supportedTypes: string[];
  };
  multimodal?: {
    nativeVision: boolean;
    imageSummaryFallback: boolean;
  };
  memory?: {
    enabled: boolean;
    defaultMode: 'auto' | 'off' | 'pinned_only' | string;
  };
}

export interface ChatMemoryRecord {
  id: string;
  sessionId?: string;
  memoryType: string;
  content: string;
  source: string;
  confidence: number;
  pinned: boolean;
  enabled: boolean;
  metadata?: Record<string, unknown> | null;
  createdAt: string;
  updatedAt: string;
}

export interface ChatArtifactEvidenceItem {
  type?: string;
  title?: string;
  url?: string;
  fileId?: string;
  filename?: string;
  sessionId?: string;
  memoryId?: string;
  path?: string;
  lineStart?: number | null;
  lineEnd?: number | null;
  snippet?: string;
  metadata?: Record<string, unknown> | null;
}

export interface AgentMemoryItem {
  id: string;
  scope: 'global' | 'app' | 'session' | 'project' | string;
  app: 'chat' | 'pm' | 'rd' | 'shared' | string;
  sessionId?: string | null;
  memoryType: string;
  content: string;
  sourceType: string;
  confidence: number;
  pinned: boolean;
  enabled: boolean;
  metadata?: Record<string, unknown> | null;
  embeddingModel?: string | null;
  embeddingDimensions?: number | null;
  createdAt: string;
  updatedAt: string;
  virtualPath?: string;
  legacySource?: string | null;
}

export interface AgentMemoryListResponse {
  items: AgentMemoryItem[];
  nextCursor?: string | null;
  hasMore: boolean;
}

export interface AgentMemoryCitation {
  id: string;
  turnId?: string | null;
  memoryId: string;
  path: string;
  lineStart?: number | null;
  lineEnd?: number | null;
  note?: string | null;
  metadata?: Record<string, unknown> | null;
  createdAt: string;
}

export interface AgentThreadMemoryState {
  sessionId: string;
  useMemories: boolean;
  generateMemories: boolean;
  pollutionState: 'clean' | 'polluted' | 'disabled' | string;
  pollutionReason?: string | null;
  lastExternalContextAt?: string | null;
}

export interface AgentContextStatus {
  sessionId: string;
  model: string;
  provider: string;
  messageCount: number;
  estimatedTokens: number;
  tokenEstimator: string;
  contextWindow: number;
  effectiveContextLimit: number;
  autoCompactTokenLimit: number;
  contextUsagePercent: number;
  tokensUntilCompaction: number;
  shouldCompact: boolean;
  unknownContextWindow: boolean;
  compactionCount: number;
  lastCompactionSummary?: string | null;
  lastCompactionRemovedMessages: number;
  state: string;
  memoryState: AgentThreadMemoryState;
}

export interface AgentManualCompactionResult {
  sessionId: string;
  windowId: string;
  trigger: string;
  strategy: string;
  summary: string;
  summaryTokens: number;
  removedMessageCount: number;
  retainedTailTokens: number;
  messageCountAfter: number;
  usedMemoryRefs: unknown[];
}

/**
 * Super_Assistant routing decision, mirrored into the session trace.
 * Aligns with the Rust `RouteDecision` (`routes::super_assistant`).
 * Validates: Requirements 2.1, 2.5, 6.6.
 */
export interface RouteDecisionEvent {
  event: 'route_decision';
  targetCapability: 'ai_chat' | 'pm_assistant' | 'nl2sql' | 'super_adversarial' | string;
  source: 'explicit_override' | 'llm_intent' | 'rule_fallback';
  /** LLM confidence for automatic decisions; null for explicit overrides. */
  confidence: number | null;
  /** Effective threshold, clamped to 0.50..=0.99 (default 0.80). */
  threshold: number;
  bypassThreshold: boolean;
  reason?: string;
  needsWebSearch?: boolean | null;
  webSearchQuery?: string | null;
  webSearchReason?: string | null;
  requiredEvidence?: Array<'web' | 'workspace' | 'code_change' | 'data_execution' | 'deep_research' | string>;
  turnId: string;
  createdAt: string;
}

/**
 * Zero_Loss_Target measurement record for a compacted session.
 * `passed` is true only when `recallRate >= threshold` (Req 4.5 / 4.6).
 */
export interface ZeroLossMeasurement {
  sessionId: string;
  /** Number of probe questions against established key facts. */
  probeCount: number;
  /** Probes still answered correctly after compaction. */
  recalledCount: number;
  /** recalledCount / probeCount. */
  recallRate: number;
  /** Pass threshold, default 0.99. */
  threshold: number;
  /** True only when recallRate >= threshold. */
  passed: boolean;
  /** Reconciled against the session_compacted event. */
  removedMessages: number;
  summaryTokens: number;
  measuredAt: string;
}

/** Default Zero_Loss_Target recall threshold (Req 4.5: 99%). */
export const ZERO_LOSS_DEFAULT_THRESHOLD = 0.99;

/**
 * Inputs for {@link computeZeroLossMeasurement}. The `removedMessages` and
 * `summaryTokens` fields are reconciled against the `session_compacted` event
 * so the measurement can be cross-checked against the actual compaction.
 */
export interface ZeroLossMeasurementInput {
  sessionId: string;
  /** Number of probe questions against established key facts. */
  probeCount: number;
  /** Probes still answered correctly after compaction. */
  recalledCount: number;
  /** Pass threshold; defaults to {@link ZERO_LOSS_DEFAULT_THRESHOLD} (0.99). */
  threshold?: number;
  /** Removed message count reported by the `session_compacted` event. */
  removedMessages: number;
  /** Summary token count reported by the `session_compacted` event. */
  summaryTokens: number;
  /** ISO timestamp of the measurement; defaults to `new Date().toISOString()`. */
  measuredAt?: string;
}

/**
 * Pure builder for a {@link ZeroLossMeasurement} from probe/recall counts.
 *
 * - `recallRate = recalledCount / probeCount`, guarded against divide-by-zero:
 *   when `probeCount === 0` there is nothing to measure, so `recallRate` is 0
 *   and the measurement cannot be judged "passed" (Req 4.6 — only mark passed
 *   when the measured recall rate actually reaches the threshold).
 * - `passed` is true if and only if `recallRate >= threshold`.
 * - Counts are clamped to sane bounds (`recalledCount` to `[0, probeCount]`,
 *   negatives floored at 0) so a malformed caller cannot produce a rate > 1.
 *
 * Validates: Requirements 4.5, 4.6.
 */
export function computeZeroLossMeasurement(
  input: ZeroLossMeasurementInput,
): ZeroLossMeasurement {
  const threshold = input.threshold ?? ZERO_LOSS_DEFAULT_THRESHOLD;
  const probeCount = Math.max(0, Math.trunc(input.probeCount));
  const recalledCount = Math.min(
    probeCount,
    Math.max(0, Math.trunc(input.recalledCount)),
  );

  // Guard divide-by-zero: no probes means no measurement, so recall is 0.
  const recallRate = probeCount === 0 ? 0 : recalledCount / probeCount;
  const passed = recallRate >= threshold;

  return {
    sessionId: input.sessionId,
    probeCount,
    recalledCount,
    recallRate,
    threshold,
    passed,
    removedMessages: Math.max(0, Math.trunc(input.removedMessages)),
    summaryTokens: Math.max(0, Math.trunc(input.summaryTokens)),
    measuredAt: input.measuredAt ?? new Date().toISOString(),
  };
}

// ---------------------------------------------------------------------------
// Super_Assistant unified message endpoint (`POST /super-assistant/messages`).
// Mirrors the Rust `routes::super_assistant` wire types (all camelCase) so the
// unified shell (`ChatCore`) can send/receive through one entry point (Req 1.1
// / 1.6 / 8.2). Reuses the `agentApi` client conventions below.
// ---------------------------------------------------------------------------

/** Origin of a Super_Assistant routing decision (mirror of Rust `RouteSource`). */
export type RouteSource = 'explicit_override' | 'llm_intent' | 'rule_fallback';

/**
 * A single routing decision made for a Super_Assistant message. Aligns with the
 * Rust `RouteDecision` (`routes::super_assistant`). Validates: Requirements 2.1.
 */
export interface RouteDecision {
  targetCapability: string;
  source: RouteSource;
  /** LLM confidence for automatic decisions; null for explicit overrides. */
  confidence: number | null;
  threshold: number;
  bypassThreshold: boolean;
  reason?: string | null;
  needsWebSearch?: boolean | null;
  webSearchQuery?: string | null;
  webSearchReason?: string | null;
  requiredEvidence?: Array<'web' | 'workspace' | 'code_change' | 'data_execution' | 'deep_research' | string>;
}

/** Kind of a key-info item retained across turns (mirror of Rust `KeyInfoKind`). */
export type KeyInfoKind =
  | 'fact'
  | 'constraint'
  | 'decision'
  | 'preference'
  | 'attachment_digest';

/** A single established key-info item (mirror of Rust `ExtractedKeyInfo`). */
export interface ExtractedKeyInfo {
  kind: KeyInfoKind;
  content: string;
  sourceTurnId: string | null;
  confidence: number;
  pinned: boolean;
}

/**
 * The session context snapshot after a Super_Assistant turn. Its `established`
 * set is a superset of the prior snapshot's (Req 2.4). Mirror of the Rust
 * `SessionContextSnapshot`.
 */
export interface SessionContextSnapshot {
  activeCapability: string | null;
  established: ExtractedKeyInfo[];
}

/** A copyable code block extracted from an `ai_chat` answer (Req 3.2). */
export interface SuperAssistantCodeBlock {
  language?: string;
  code: string;
}

/** The `ai_chat` capability answer (Req 3.1 / 3.2). */
export interface SuperAssistantChatAnswer {
  answer: string;
  codeBlocks: SuperAssistantCodeBlock[];
  isCodeAnswer: boolean;
  model: string;
}

/** The `nl2sql` SQL-troubleshooting conclusion (Req 3.6). */
export interface SuperAssistantSqlConclusion {
  correctedSql?: string | null;
  conclusion: string;
  clarificationQuestion?: string;
  queryId?: string;
  resolved: boolean;
}

/** Which async deep-analysis link a request was delegated to (Req 3.7). */
export type SuperAssistantDeepAnalysisLink =
  | 'pmResearchTask'
  | 'chatAdversarialRun'
  | 'dataAttributionTask';

/** The async deep-analysis task handle returned for a routed message (Req 3.7). */
export interface SuperAssistantDeepAnalysisHandle {
  link: SuperAssistantDeepAnalysisLink;
  taskId: string;
  sessionId?: string;
  status: string;
}

/**
 * The capability answer produced for a Super_Assistant message. Tagged union
 * mirroring the Rust `SuperAssistantAnswerPayload` (`{ kind, answer }`).
 */
export type SuperAssistantAnswer =
  | { kind: 'chat'; answer: SuperAssistantChatAnswer }
  | { kind: 'sql'; answer: SuperAssistantSqlConclusion }
  | { kind: 'deepAnalysis'; answer: SuperAssistantDeepAnalysisHandle };

/** Request body for `POST /super-assistant/messages`. */
export interface SuperAssistantMessageRequest {
  sessionId: string;
  text: string;
  /** Exact user-visible text when a slash command is stripped from `text`. */
  displayText?: string;
  turnId?: string;
  explicitCapability?: string;
  app?: string;
  model?: string;
  dataSourceId?: string;
  dataAttribution?: boolean;
  routerConfig?: Record<string, unknown>;
}

/** Response body for `POST /super-assistant/messages`. */
export interface SuperAssistantMessageResponse {
  decision: RouteDecision;
  routeEvent: RouteDecisionEvent;
  persistedRouteEvents: number;
  context: SessionContextSnapshot;
  /**
   * The production Zero_Loss_Target measurement, present only when a compaction
   * ran during this turn (Req 4.5 / 4.6). Mirrors the backend
   * `record_zero_loss_measurement` output and the pure
   * {@link computeZeroLossMeasurement} shape.
   */
  zeroLoss?: ZeroLossMeasurement | null;
  answer: SuperAssistantAnswer;
}

export interface SuperAssistantActiveTurnResponse {
  active: boolean;
  link?: 'superAssistantTurn' | 'pmResearchTask' | 'chatAdversarialRun' | 'dataAttributionTask' | string;
  turnId?: string;
  taskId?: string;
  status?: string;
}

export interface ChatFileRecord {
  id: string;
  fileId: string;
  filename: string;
  mediaType: string;
  size: number;
  url: string;
  status: 'uploaded' | 'parsing' | 'indexed' | 'failed' | string;
  errorMessage?: string | null;
  chunkCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface CancelSessionTurnResponse {
  sessionId: string;
  cancelled: boolean;
  status: 'cancelling' | 'idle';
}

export interface CancelSuperAssistantTurnResponse {
  turnId: string;
  sessionId: string;
  status: string;
  cancelled: boolean;
}

/** Monotonically increasing sequence number for SSE request deduplication.
 *
 * Incremented every time `streamAgentSession` is called. Used to detect and
 * discard stale SSE events from an aborted previous request whose stream delivers
 * late (e.g. slow primary API key that finally responds after a fallback key
 * already handled a subsequent question). */
let _streamAgentSeq = 0;

export function createSuperAssistantTurnId(): string {
  const randomUuid = globalThis.crypto?.randomUUID?.();
  if (randomUuid) return randomUuid;
  return `super-assistant-${Date.now()}-${Math.random().toString(36).slice(2, 12)}`;
}

// ---- Agent ----
export const agentApi = {
  /** Create a new agent session */
  createSession: (data?: { project_id?: string; model?: string; source?: string; scenario?: string; locale?: string }) =>
    fastClient.post<{ session: AgentSession }>('/agent/sessions', data ?? {}).then((r) => r.data),

  /** List active agent sessions, optionally filtered by source (e.g. "chat" or "agent") */
  listSessions: (source?: string) =>
    fastClient
      .get<{ sessions: AgentSessionInfo[]; total: number }>(
        '/agent/sessions',
        source ? { params: { source } } : undefined
      )
      .then((r) => r.data),

  /** Get a specific session */
  getSession: (sessionId: string) =>
    fastClient.get<AgentSession>(`/agent/sessions/${encodeURIComponent(sessionId)}`).then((r) => r.data),

  listSessionMemoryCitations: (sessionId: string) =>
    fastClient
      .get<{ sessionId: string; items: AgentMemoryCitation[] }>(
        `/agent/sessions/${encodeURIComponent(sessionId)}/memory-citations`,
      )
      .then((r) => r.data),

  getChatSessionEvidence: (sessionId: string) =>
    fastClient
      .get<{ sessionId: string; items: ChatArtifactEvidenceItem[] }>(
        `/chat/sessions/${encodeURIComponent(sessionId)}/evidence`,
      )
      .then((r) => r.data),

  getChatCapabilities: (model?: string) =>
    fastClient
      .get<ChatCapabilityResponse>('/chat/capabilities', model ? { params: { model } } : undefined)
      .then((r) => r.data),

  listChatMemories: () =>
    fastClient
      .get<{ items: ChatMemoryRecord[]; paused: boolean; defaultMode: string }>('/chat/memories')
      .then((r) => r.data),

  createChatMemory: (data: { content: string; memoryType?: string; pinned?: boolean; enabled?: boolean; metadata?: Record<string, unknown> }) =>
    fastClient.post<ChatMemoryRecord>('/chat/memories', data).then((r) => r.data),

  updateChatMemory: (id: string, data: { content: string; memoryType?: string; pinned?: boolean; enabled?: boolean; metadata?: Record<string, unknown> }) =>
    fastClient.patch<ChatMemoryRecord>(`/chat/memories/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteChatMemory: (id: string) =>
    fastClient.delete<{ deleted: boolean }>(`/chat/memories/${encodeURIComponent(id)}`).then((r) => r.data),

  pauseChatMemory: (paused: boolean) =>
    fastClient.post<{ paused: boolean; defaultMode: string }>('/chat/memories/pause', { paused }).then((r) => r.data),

  getChatMemorySettings: () =>
    fastClient.get<{ paused: boolean; defaultMode: string }>('/chat/memories/pause').then((r) => r.data),

  listPmSessionMemories: (sessionId: string) =>
    fastClient
      .get<{
        items: ChatMemoryRecord[];
        paused: boolean;
        defaultMode: string;
        summary?: {
          summary: string;
          turnCount: number;
          sourceTaskId?: string | null;
          lastCompactedRemovedMessages?: number | null;
          metadata?: Record<string, unknown> | null;
          updatedAt: string;
        } | null;
      }>(`/agent/sessions/${encodeURIComponent(sessionId)}/pm-memories`)
      .then((r) => r.data),

  createPmSessionMemory: (sessionId: string, data: { content: string; memoryType?: string; pinned?: boolean; enabled?: boolean; metadata?: Record<string, unknown> }) =>
    fastClient.post<ChatMemoryRecord>(`/agent/sessions/${encodeURIComponent(sessionId)}/pm-memories`, data).then((r) => r.data),

  updatePmSessionMemory: (sessionId: string, id: string, data: { content: string; memoryType?: string; pinned?: boolean; enabled?: boolean; metadata?: Record<string, unknown> }) =>
    fastClient.patch<ChatMemoryRecord>(`/agent/sessions/${encodeURIComponent(sessionId)}/pm-memories/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deletePmSessionMemory: (sessionId: string, id: string) =>
    fastClient.delete<{ deleted: boolean }>(`/agent/sessions/${encodeURIComponent(sessionId)}/pm-memories/${encodeURIComponent(id)}`).then((r) => r.data),

  pausePmSessionMemory: (sessionId: string, paused: boolean) =>
    fastClient.post<{ paused: boolean; defaultMode: string }>(`/agent/sessions/${encodeURIComponent(sessionId)}/pm-memories/pause`, { paused }).then((r) => r.data),

  getPmSessionMemorySettings: (sessionId: string) =>
    fastClient.get<{ paused: boolean; defaultMode: string }>(`/agent/sessions/${encodeURIComponent(sessionId)}/pm-memories/pause`).then((r) => r.data),

  listUnifiedMemories: (params?: {
    app?: string;
    sessionId?: string;
    includeLegacy?: boolean;
    sourceGroup?: 'manual' | 'automatic';
    cursor?: string | null;
    limit?: number;
  }) =>
    fastClient
      .get<AgentMemoryListResponse>('/memory/items', {
        params: {
          app: params?.app,
          sessionId: params?.sessionId,
          includeLegacy: params?.includeLegacy ?? true,
          sourceGroup: params?.sourceGroup,
          cursor: params?.cursor || undefined,
          limit: params?.limit,
        },
      })
      .then((r) => r.data),

  createUnifiedMemory: (data: {
    content: string;
    app?: string;
    scope?: string;
    sessionId?: string | null;
    memoryType?: string;
    sourceType?: string;
    pinned?: boolean;
    enabled?: boolean;
    metadata?: Record<string, unknown>;
  }) => fastClient.post<AgentMemoryItem>('/memory/items', data).then((r) => r.data),

  deleteUnifiedMemory: (id: string) =>
    fastClient.delete<{ deleted: boolean }>(`/memory/items/${encodeURIComponent(id)}`).then((r) => r.data),

  getSessionContextStatus: (sessionId: string) =>
    fastClient
      .get<AgentContextStatus>(`/agent/sessions/${encodeURIComponent(sessionId)}/context-status`)
      .then((r) => r.data),

  compactSessionContext: (sessionId: string) =>
    client
      .post<AgentManualCompactionResult>(`/agent/sessions/${encodeURIComponent(sessionId)}/compact`, {})
      .then((r) => r.data),

  updateSessionMemoryMode: (
    sessionId: string,
    data: {
      useMemories?: boolean;
      generateMemories?: boolean;
      pollutionState?: 'clean' | 'polluted' | 'disabled' | string;
      pollutionReason?: string | null;
    },
  ) =>
    fastClient
      .patch<AgentThreadMemoryState>(`/agent/sessions/${encodeURIComponent(sessionId)}/memory-mode`, data)
      .then((r) => r.data),

  registerChatFile: (data: { fileId: string; filename: string; mediaType: string; size?: number; url: string; sessionId?: string | null }) =>
    fastClient.post<ChatFileRecord>('/chat/files', data).then((r) => r.data),

  listChatFiles: (params?: { sessionId?: string }) =>
    fastClient.get<{ items: ChatFileRecord[]; total: number }>('/chat/files', { params }).then((r) => r.data),

  getChatFileStatus: (fileId: string) =>
    fastClient.get<ChatFileRecord>(`/chat/files/${encodeURIComponent(fileId)}/status`).then((r) => r.data),

  searchChatFiles: (data: { query: string; fileIds?: string[]; limit?: number; strictGrounding?: boolean }) =>
    fastClient.post('/chat/files/search', data).then((r) => r.data),

  getChatSessionTrace: (sessionId: string) =>
    fastClient.get<{ sessionId: string; items: Array<Record<string, unknown>> }>(`/chat/sessions/${encodeURIComponent(sessionId)}/trace`).then((r) => r.data),

  /** Delete a session */
  deleteSession: (sessionId: string) =>
    fastClient.delete(`/agent/sessions/${encodeURIComponent(sessionId)}`).then((r) => r.data),

  /** Rename a session */
  renameSession: (sessionId: string, name: string) =>
    fastClient.patch<{ renamed: boolean; name: string }>(
      `/agent/sessions/${encodeURIComponent(sessionId)}`,
      { name }
    ).then((r) => r.data),

  /** Run a turn (non-streaming) */
  runTurn: (sessionId: string, message: string, cwd?: string) =>
    client.post<AgentTurnResult>(
      `/agent/sessions/${encodeURIComponent(sessionId)}/turn`,
      { message, cwd }
    ).then((r) => r.data),

  /** Request real backend cancellation for the active streaming turn. */
  cancelSessionTurn: (sessionId: string) =>
    fastClient
      .post<CancelSessionTurnResponse>(
        `/agent/sessions/${encodeURIComponent(sessionId)}/cancel-turn`,
        {},
      )
      .then((r) => r.data),

  startChatAdversarialRun: (data: {
    question: string;
    models: string[];
    max_rounds?: number;
    parent_run_id?: string;
  }) =>
    fastClient
      .post<ChatAdversarialRun>('/agent/chat-adversarial-runs', data)
      .then((r) => r.data),

  listChatAdversarialRuns: (params?: { limit?: number; page?: number; per_page?: number }) =>
    fastClient
      .get<{
        items: ChatAdversarialRun[];
        total: number;
        page: number;
        per_page: number;
        has_more: boolean;
      }>('/agent/chat-adversarial-runs', { params })
      .then((r) => r.data),

  getChatAdversarialRun: (runId: string) =>
    fastClient
      .get<ChatAdversarialRun>(`/agent/chat-adversarial-runs/${encodeURIComponent(runId)}`)
      .then((r) => r.data),

  cancelChatAdversarialRun: (runId: string) =>
    fastClient
      .post<{ ok: boolean; run_id: string; status: string }>(
        `/agent/chat-adversarial-runs/${encodeURIComponent(runId)}/cancel`,
        {},
      )
      .then((r) => r.data),

  getChatAdversarialThread: (
    runId: string,
    params?: { limit?: number; page?: number; per_page?: number }
  ) =>
    fastClient
      .get<{
        thread_id: string;
        total: number;
        page: number;
        per_page: number;
        has_more: boolean;
        items: ChatAdversarialRun[];
      }>(
        `/agent/chat-adversarial-runs/${encodeURIComponent(runId)}/thread`,
        { params }
      )
      .then((r) => r.data),

  updateChatAdversarialThread: (
    runId: string,
    data: { title?: string; is_pinned?: boolean }
  ) =>
    fastClient
      .patch<ChatAdversarialRun>(
        `/agent/chat-adversarial-runs/${encodeURIComponent(runId)}/thread`,
        data
      )
      .then((r) => r.data),

  deleteChatAdversarialThread: (runId: string) =>
    fastClient
      .delete<{ deleted: boolean; thread_id: string }>(
        `/agent/chat-adversarial-runs/${encodeURIComponent(runId)}/thread`
      )
      .then((r) => r.data),

  /** Start background PM deep-research task for a PM session. */
	  startPmResearchTask: (
	    sessionId: string,
	    message: string,
	    images?: PmTaskImageInput[],
	    documents?: PmTaskDocumentInput[],
	  ) =>
	    fastClient
	      .post<PmResearchTaskStartResponse>(
	        `/agent/sessions/${encodeURIComponent(sessionId)}/pm-research-tasks`,
	        { message, images: images ?? [], documents: documents ?? [] },
	      )
      .then((r) => r.data),

  /** Get background PM research task status snapshot. */
  getPmResearchTaskStatus: (taskId: string) =>
    fastClient
      .get<PmResearchTaskStatusResponse>(
        `/agent/pm-research-tasks/${encodeURIComponent(taskId)}`,
      )
      .then((r) => r.data),

  /** List subtask runtime snapshots for a PM research task. */
  getPmResearchTaskSubtasks: (taskId: string) =>
    fastClient
      .get<{ taskId: string; items: PmSubtaskRuntimeRow[]; count: number }>(
        `/agent/pm-research-tasks/${encodeURIComponent(taskId)}/subtasks`,
      )
      .then((r) => r.data),

  /** List attempt rows for a subtask. */
  getPmResearchTaskSubtaskAttempts: (taskId: string, subtaskId: string) =>
    fastClient
      .get<{ taskId: string; subtaskId: string; items: PmSubtaskAttemptRow[]; count: number }>(
        `/agent/pm-research-tasks/${encodeURIComponent(taskId)}/subtasks/${encodeURIComponent(subtaskId)}/attempts`,
      )
      .then((r) => r.data),

  /** Request cancellation for a running background PM research task. */
  cancelPmResearchTask: (taskId: string) =>
    fastClient
      .post<PmResearchTaskControlResponse>(
        `/agent/pm-research-tasks/${encodeURIComponent(taskId)}/cancel`,
        {},
      )
      .then((r) => r.data),

  /** Resume a failed/cancelled background PM research task. */
  resumePmResearchTask: (taskId: string) =>
    fastClient
      .post<PmResearchTaskControlResponse>(
        `/agent/pm-research-tasks/${encodeURIComponent(taskId)}/resume`,
        {},
      )
      .then((r) => r.data),

  /** Record tenant-level PM retrieval strategy outcome for online ranking. */
  recordPmStrategyOutcome: (data: PmStrategyRecordRequest) =>
    fastClient
      .post<{ ok: boolean }>('/agent/pm-strategy-records', data)
      .then((r) => r.data),

  /** Fetch tenant-level PM strategy leaderboard. */
  listPmStrategyLeaderboard: () =>
    fastClient
      .get<PmStrategyLeaderboardResponse>('/agent/pm-strategy-leaderboard')
      .then((r) => r.data),

  /** List tenant PM runtime budget profiles. */
  listPmBudgetProfiles: () =>
    fastClient
      .get<PmBudgetProfilesResponse>('/agent/pm-budget-profiles')
      .then((r) => r.data),

  /** Activate a PM budget profile for the current tenant. */
  activatePmBudgetProfile: (profileKey: string) =>
    fastClient
      .post<PmBudgetProfileActivateResponse>('/agent/pm-budget-profiles/activate', {
        profileKey,
      })
      .then((r) => r.data),

  /** Get PM SLO summary windows (default 7d + 30d). */
  listPmSloSummary: (days?: number) =>
    fastClient
      .get<PmSloSummaryResponse>('/agent/pm-slo-summary', {
        params: days ? { days } : undefined,
      })
      .then((r) => r.data),

  /** Get PM failure taxonomy grouped by error code. */
  listPmFailureTaxonomy: (params?: { days?: number; limit?: number }) =>
    fastClient
      .get<PmFailureTaxonomyResponse>('/agent/pm-failure-taxonomy', { params })
      .then((r) => r.data),

  /** PM provider/channel health summary. */
  listPmProviderHealth: (limit?: number) =>
    fastClient
      .get<PmProviderHealthResponse>('/agent/pm-provider-health', {
        params: limit ? { limit } : undefined,
      })
      .then((r) => r.data),

  /** PM route learning feature summary. */
  listPmRouteLearningFeatures: (params?: { page?: number; per_page?: number }) =>
    fastClient
      .get<PmRouteLearningFeaturesResponse>('/agent/pm-route-learning-features', {
        params,
      })
      .then((r) => r.data),

  /** PM quality-gate summary windows (default 7d + 30d). */
  listPmQualityGateSummary: (days?: number) =>
    fastClient
      .get<PmQualityGateSummaryResponse>('/agent/pm-quality-gate-summary', {
        params: days ? { days } : undefined,
      })
      .then((r) => r.data),

  /** PM knowledge coverage warnings with summary. */
  listPmKnowledgeCoverageWarnings: (params?: { days?: number; limit?: number; page?: number; per_page?: number }) =>
    fastClient
      .get<PmKnowledgeCoverageWarningsResponse>('/agent/pm-knowledge-coverage-warnings', { params })
      .then((r) => r.data),

  /** PM runtime pressure, retry recovery and source quota exhaustion insights. */
  listPmRuntimeInsights: (params?: { days?: number }) =>
    fastClient
      .get<PmRuntimeInsightsResponse>('/agent/pm-runtime-insights', { params })
      .then((r) => r.data),

  /** Get session state */
  getSessionState: (sessionId: string) =>
    fastClient
      .get<{
        state: string;
        canonical?: {
          sessionId: string;
          entry: 'super_assistant' | 'agent' | string;
          source: string;
          running: boolean;
          resumable: boolean;
          historyEndpoint: string;
          streamEndpoint: string;
          cancelEndpoint: string;
        };
      }>(`/agent/sessions/${encodeURIComponent(sessionId)}/state`)
      .then((r) => r.data),

  /** Get session message history (turn-cursor pagination). */
  getSessionHistory: (
    sessionId: string,
    params?: {
      before_turn_cursor?: number;
      limit_turns?: number;
      max_bytes?: number;
    },
  ) =>
    fastClient
      .get<AgentSessionHistory>(
        `/agent/sessions/${encodeURIComponent(sessionId)}/history`,
        params ? { params } : undefined,
      )
      .then((r) => r.data),

  /** Toggle pin state of a session */
  togglePinSession: (sessionId: string) =>
    fastClient.post<{ pinned: boolean; is_pinned: boolean }>(
      `/agent/sessions/${encodeURIComponent(sessionId)}/pin`
    ).then((r) => r.data),

  /** Toggle bookmark state of a session */
  toggleBookmarkSession: (sessionId: string) =>
    fastClient.post<{ bookmarked: boolean }>(
      `/agent/sessions/${encodeURIComponent(sessionId)}/bookmark`
    ).then((r) => r.data),

  /** Create a new session branched from a specific message in another session */
  branchSession: (sessionId: string, messageIndex: number, messageId?: number) =>
    fastClient.post<{ session: AgentSession }>(
      `/agent/sessions/${encodeURIComponent(sessionId)}/branch`,
      { message_index: messageIndex, message_id: messageId }
    ).then((r) => r.data),

  /**
   * Process one Super_Assistant message end-to-end through the unified entry
   * point (`POST /super-assistant/messages`): server-side intent routing →
   * capability dispatch → memory/compaction → retrieval injection → answer
   * (Req 1.1 / 1.6 / 8.2). Uses the slower `client` because the call is backed
   * by a model turn, mirroring {@link agentApi.runTurn}.
   */
  sendSuperAssistantMessage: (data: SuperAssistantMessageRequest) =>
    client
      .post<SuperAssistantMessageResponse>('/super-assistant/messages', data)
      .then((r) => r.data),

  /** Return the running Super Assistant turn for a session, if any. */
  getSuperAssistantActiveTurn: (sessionId: string) =>
    fastClient
      .get<SuperAssistantActiveTurnResponse>('/super-assistant/turns/active', {
        params: { sessionId },
      })
      .then((r) => r.data),

  /** Cancel the durable parent turn and all specialist subtasks it owns. */
  cancelSuperAssistantTurn: (turnId: string) =>
    fastClient
      .post<CancelSuperAssistantTurnResponse>(
        `/super-assistant/turns/${encodeURIComponent(turnId)}/cancel`,
        {},
      )
      .then((r) => r.data),
};

/** SSE streaming for agent sessions */
export function streamAgentSession(
  sessionId: string,
  message: string,
  handlers: {
    onSessionActivated?: (meta: import('@/types').SessionMetadata) => void;
    /** Emitted when the runtime was hot-reloaded (MCP/Skills changed mid-session). */
    onConfigHotReload?: (meta: import('@/types').SessionMetadata) => void;
    /** Emitted when the session was auto-compacted. */
    onSessionCompacted?: (removedMessages: number, summary: string) => void;
    onText?: (text: string) => void;
    /** Emitted when a thinking/reasoning block starts. */
    onThinkingStart?: (index: number) => void;
    /** Emitted for each delta of thinking/reasoning content. */
    onThinkingDelta?: (text: string) => void;
    /** Emitted when the thinking/reasoning block ends. */
    onThinkingEnd?: (index: number) => void;
    /** Emitted when a text block starts. */
    onTextBlockStart?: (index: number) => void;
    /** Emitted when a text block ends. */
    onTextBlockEnd?: (index: number) => void;
    /** Tool call started (name + id known). */
    onToolUseStart?: (index: number, id: string, name: string) => void;
    /** Tool call input JSON delta. */
    onToolInputDelta?: (index: number, partialJson: string) => void;
    /** Tool call execution completed. */
    onToolUseEnd?: (index: number) => void;
    /** Tool execution result from the executor. Includes tool name and input from the runtime. */
    onToolResult?: (index: number, toolName: string, input: string, output: string, isError: boolean, durationMs?: number) => void;
    /** Tool call completed (legacy — single event with full data, sent at turn end). */
    onToolCall?: (tool: AgentToolCall) => void;
    onUsage?: (usage: AgentUsage) => void;
    onPmStage?: (stage: {
      stage: string;
      status: string;
      attempt?: number;
      detail?: Record<string, unknown>;
    }) => void;
    onPmQuality?: (quality: {
      passed: boolean;
      deliverable?: boolean;
      quality_level?: string;
      has_tool_calls: boolean;
      tool_call_count: number;
      citation_count: number;
      domain_count?: number;
      claim_count?: number;
      claim_alignment_ok?: boolean;
      citations?: string[];
      domains?: string[];
      claim_alignment?: Array<{
        claim: string;
        evidence_excerpt?: string;
        urls: string[];
        cited: boolean;
      }>;
      evidence_tree?: Array<{
        claim: string;
        status: string;
        evidence_count: number;
        evidences: Array<{
          url: string;
          domain: string;
          excerpt: string;
        }>;
      }>;
      conflict_matrix?: Array<{
        topic: string;
        source_a: string;
        claim_a: string;
        source_b: string;
        claim_b: string;
        verdict: string;
      }>;
      conflict_graph?: {
        topic_count: number;
        edge_count: number;
        adjudicated_count: number;
        unresolved_count: number;
        avg_confidence: number;
        edges: Array<{
          topic: string;
          source_left: string;
          source_right: string;
          relation: string;
          verdict: string;
          confidence: number;
          urls: string[];
        }>;
      };
      missing: string[];
      suggestions: string[];
    }) => void;
    onStreamEnd?: (
      iterations: number,
      usage?: AgentUsage,
      fullText?: string,
      finalThinking?: string,
      meta?: {
        pm_quality?: {
          passed: boolean;
          deliverable?: boolean;
          quality_level?: string;
          has_tool_calls: boolean;
          tool_call_count: number;
          citation_count: number;
          domain_count?: number;
          claim_count?: number;
          claim_alignment_ok?: boolean;
          citations?: string[];
          domains?: string[];
          claim_alignment?: Array<{
            claim: string;
            evidence_excerpt?: string;
            urls: string[];
            cited: boolean;
          }>;
          evidence_tree?: Array<{
            claim: string;
            status: string;
            evidence_count: number;
            evidences: Array<{
              url: string;
              domain: string;
              excerpt: string;
            }>;
          }>;
          conflict_matrix?: Array<{
            topic: string;
            source_a: string;
            claim_a: string;
            source_b: string;
            claim_b: string;
            verdict: string;
          }>;
          conflict_graph?: {
            topic_count: number;
            edge_count: number;
            adjudicated_count: number;
            unresolved_count: number;
            avg_confidence: number;
            edges: Array<{
              topic: string;
              source_left: string;
              source_right: string;
              relation: string;
              verdict: string;
              confidence: number;
              urls: string[];
            }>;
          };
          missing: string[];
          suggestions: string[];
        };
        pm_report?: Record<string, unknown>;
        streamMode?: 'token_delta' | 'tool_interleaved' | 'final_text_typewriter' | string;
        telemetry?: {
          firstTokenLatencyMs?: number | null;
          elapsedMs?: number;
          fallbackTypewriter?: boolean;
          textDeltaCount?: number;
          toolInterleaved?: boolean;
          cancelled?: boolean;
          searchMode?: string;
        };
      },
    ) => void;
    /** Non-blocking warning emitted when image context couldn't be fully processed. */
    onImageContextWarning?: (payload: { message: string; detail?: string; code?: string }) => void;
    /** Super Assistant route result for non-chat capabilities such as PM research or data attribution. */
    onSuperAssistantAnswer?: (answer: SuperAssistantAnswer) => void;
    /** Available synchronously for durable cancellation before the first SSE event arrives. */
    onSuperAssistantTurnId?: (turnId: string) => void;
    onError?: (error: string) => void;
  },
  options?: {
    images?: PmTaskImageInput[];
    documents?: PmTaskDocumentInput[];
    turnOptions?: ChatTurnOptions;
    superAssistant?: {
      app?: string;
      model?: string;
      displayText?: string;
      explicitCapability?: string;
      dataSourceId?: string;
      dataAttribution?: boolean;
      routerConfig?: Record<string, unknown>;
    };
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  // Monotonically increasing sequence number. Incremented each time streamAgentSession
  // is called. Used to detect and discard stale events from an aborted previous request
  // whose SSE stream delivers late (e.g. slow primary API key that finally responds
  // after the fallback key already handled a subsequent question).
  let requestSeq = ++_streamAgentSeq;
  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let resumedStreamAbort: (() => void) | null = null;
  let superAssistantTurnId = options?.superAssistant
    ? createSuperAssistantTurnId()
    : undefined;
  if (superAssistantTurnId) {
    handlers.onSuperAssistantTurnId?.(superAssistantTurnId);
  }
  let lastEventSeq = 0;
  let currentEventSeq = 0;
  let latestUsage: AgentUsage | undefined;
  let terminalEventSeen = false;
  let superAssistantEventState = createSuperAssistantEventState();
  let currentEvent: string = '';
  let currentData: string = '';
  // Accumulate all text deltas for this stream so we can pass them to onStreamEnd
  // without depending on the caller's mutable blocksRef.
  let textDeltas: string[] = [];
  // Capture the final text from the non-streaming text event (turn-end summary).
  // This is the authoritative full text; prefer it over textDeltas when available.
  let finalText: string | undefined;
  // Accumulate thinking/reasoning deltas for streaming display.
  let thinkingDeltas: string[] = [];
  // PM research quality report emitted near stream end.
  let latestPmQuality:
      | {
          passed: boolean;
          deliverable?: boolean;
          quality_level?: string;
          has_tool_calls: boolean;
        tool_call_count: number;
        citation_count: number;
        domain_count?: number;
        claim_count?: number;
        claim_alignment_ok?: boolean;
        citations?: string[];
        domains?: string[];
        claim_alignment?: Array<{
          claim: string;
          evidence_excerpt?: string;
          urls: string[];
          cited: boolean;
        }>;
        evidence_tree?: Array<{
          claim: string;
          status: string;
          evidence_count: number;
          evidences: Array<{
            url: string;
            domain: string;
            excerpt: string;
          }>;
        }>;
        conflict_matrix?: Array<{
          topic: string;
          source_a: string;
          claim_a: string;
          source_b: string;
          claim_b: string;
          verdict: string;
        }>;
        conflict_graph?: {
          topic_count: number;
          edge_count: number;
          adjudicated_count: number;
          unresolved_count: number;
          avg_confidence: number;
          edges: Array<{
            topic: string;
            source_left: string;
            source_right: string;
            relation: string;
            verdict: string;
            confidence: number;
            urls: string[];
          }>;
        };
        missing: string[];
        suggestions: string[];
      }
    | undefined;
  let latestPmReport: Record<string, unknown> | undefined;
  // Typewriter queue for smoother incremental rendering.
  // Network chunks often contain many SSE events in one read cycle; directly
  // pushing each delta can be React-batched into a large jump. Queue + tick
  // keeps the output visually continuous.
  let pendingTextQueue = '';
  let textFlushTimer: ReturnType<typeof setInterval> | null = null;
  const TYPEWRITER_CHUNK_SIZE = 18;
  const TYPEWRITER_TICK_MS = 22;
  const TYPEWRITER_BACKLOG_FLUSH_CHARS = 2048;
  const stopTextFlushTimer = () => {
    if (textFlushTimer != null) {
      clearInterval(textFlushTimer);
      textFlushTimer = null;
    }
  };
  const flushNextTextChunk = () => {
    if (aborted) {
      pendingTextQueue = '';
      stopTextFlushTimer();
      return;
    }
    if (!pendingTextQueue) {
      stopTextFlushTimer();
      return;
    }
    const nextChunk = pendingTextQueue.slice(0, TYPEWRITER_CHUNK_SIZE);
    pendingTextQueue = pendingTextQueue.slice(nextChunk.length);
    handlers.onText?.(nextChunk);
    if (!pendingTextQueue) {
      stopTextFlushTimer();
    }
  };
  const ensureTextFlushTimer = () => {
    if (textFlushTimer == null) {
      textFlushTimer = setInterval(flushNextTextChunk, TYPEWRITER_TICK_MS);
    }
  };
  const enqueueTextDelta = (text: string) => {
    if (!text) return;
    pendingTextQueue += text;
    // Background tabs heavily throttle timers. Keeping the cosmetic typewriter
    // queue active there makes a completed answer replay character-by-character
    // when the user returns. Raw stream state is authoritative, so flush it in
    // one update while hidden or whenever a burst has built a large backlog.
    if (
      (typeof document !== 'undefined' && document.hidden) ||
      pendingTextQueue.length >= TYPEWRITER_BACKLOG_FLUSH_CHARS
    ) {
      drainTextQueue();
      return;
    }
    ensureTextFlushTimer();
  };
  const drainTextQueue = () => {
    if (pendingTextQueue) {
      handlers.onText?.(pendingTextQueue);
      pendingTextQueue = '';
    }
    stopTextFlushTimer();
  };
  const resolveStreamText = () => {
    const deltaText = textDeltas.join('');
    if (!finalText) return deltaText;
    if (deltaText.length > finalText.length && deltaText.endsWith(finalText)) {
      return deltaText;
    }
    return finalText;
  };
  const resumePersistedSuperAssistantTurn = () => {
    if (
      aborted ||
      resumedStreamAbort ||
      !options?.superAssistant ||
      !superAssistantTurnId
    ) {
      return false;
    }
    drainTextQueue();
    resumedStreamAbort = streamSuperAssistantTurnEvents(
      superAssistantTurnId,
      handlers,
      { afterSeq: lastEventSeq, sessionId, reducerState: superAssistantEventState },
    );
    return true;
  };

  const streamUrl = options?.superAssistant
    ? `${baseUrl}/api/v1/super-assistant/messages/stream`
    : `${baseUrl}/api/v1/agent/sessions/${encodeURIComponent(sessionId)}/stream`;
  const streamBody = options?.superAssistant
    ? {
        sessionId,
        turnId: superAssistantTurnId,
        text: message,
        images: options.images ?? [],
        documents: options.documents ?? [],
        ...(options.superAssistant.displayText
          ? { displayText: options.superAssistant.displayText }
          : {}),
        app: options.superAssistant.app ?? 'chat',
        ...(options.superAssistant.model ? { model: options.superAssistant.model } : {}),
        ...(options.superAssistant.explicitCapability
          ? { explicitCapability: options.superAssistant.explicitCapability }
          : {}),
        ...(options.superAssistant.dataSourceId
          ? { dataSourceId: options.superAssistant.dataSourceId }
          : {}),
        ...(options.superAssistant.dataAttribution
          ? { dataAttribution: true }
          : {}),
        ...(options.superAssistant.routerConfig
          ? { routerConfig: options.superAssistant.routerConfig }
          : {}),
      }
    : {
        message,
        images: options?.images ?? [],
        documents: options?.documents ?? [],
        turnOptions: options?.turnOptions ?? {},
      };

  // POST avoids URL length limits for long messages and hides the prompt from logs.
  fetch(streamUrl, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
    body: JSON.stringify(streamBody),
  }).then(async (response) => {
    if (!response.ok) {
      const text = await response.text();
      if (response.status >= 500 && resumePersistedSuperAssistantTurn()) return;
      handlers.onError?.(`Request failed: ${response.status} ${text}`);
      return;
    }

    const stream = response.body;
    if (!stream) {
      handlers.onError?.('Empty response body');
      return;
    }

    reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    const flush = () => {
      if (!currentEvent || !currentData) {
        currentEvent = '';
        currentData = '';
        return;
      }
      // Drop events from a stale request (aborted + superseded by a newer requestSeq).
      // Check this FIRST — before parsing — so that handler calls never happen for stale events.
      // Previously this check was after JSON.parse, meaning handlers like onToolResult could
      // mutate shared state (toolCallsRef) before the stale event was discarded.
      if (requestSeq !== _streamAgentSeq) {
        pendingTextQueue = '';
        stopTextFlushTimer();
        currentEvent = '';
        currentData = '';
        currentEventSeq = 0;
        finalText = undefined;
        thinkingDeltas = [];
        textDeltas = [];
        latestUsage = undefined;
        return;
      }
      try {
        const data = JSON.parse(currentData);
        if (options?.superAssistant) {
          const reduced = reduceSuperAssistantEvent(superAssistantEventState, {
            id: currentEventSeq,
            event: currentEvent,
            data,
          });
          superAssistantEventState = reduced.state;
          applySuperAssistantEventEffects(
            reduced.effects,
            superAssistantEventState,
            handlers,
            enqueueTextDelta,
            drainTextQueue,
            (turnId) => {
              superAssistantTurnId = turnId;
              handlers.onSuperAssistantTurnId?.(turnId);
            },
          );
          lastEventSeq = superAssistantEventState.lastEventId;
          terminalEventSeen = superAssistantEventState.terminal;
          finalText = superAssistantEventState.finalText;
          textDeltas = [...superAssistantEventState.textDeltas];
          thinkingDeltas = [...superAssistantEventState.thinkingDeltas];
          latestUsage = superAssistantEventState.usage as AgentUsage | undefined;
          latestPmQuality = superAssistantEventState.pmQuality as typeof latestPmQuality;
          latestPmReport = superAssistantEventState.pmReport;
          currentEvent = '';
          currentData = '';
          currentEventSeq = 0;
          return;
        }
        if (currentEvent === 'route_decision') {
          const routeData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          const turnId = routeData?.turnId ?? routeData?.turn_id;
          if (typeof turnId === 'string' && turnId.trim()) {
            superAssistantTurnId = turnId.trim();
          }
        } else if (currentEvent === 'ping') {
          // Keep-alive heartbeat. It only keeps the HTTP/SSE path warm and
          // should not mutate visible chat state.
        } else if (currentEvent === 'session_activated') {
          // The backend sends AgentEvent::SessionActivated { mcp_servers, skills, ... }
          // which serializes as { "SessionActivated": { "mcp_servers": [...], ... } }.
          const inner = (typeof data === 'object' && !Array.isArray(data) && 'SessionActivated' in data)
            ? data['SessionActivated']
            : data;
          handlers.onSessionActivated?.(inner as import('@/types').SessionMetadata);
        } else if (currentEvent === 'config_hot_reload') {
          const inner = (typeof data === 'object' && !Array.isArray(data) && 'ConfigHotReload' in data)
            ? data['ConfigHotReload']
            : data;
          handlers.onConfigHotReload?.(inner as import('@/types').SessionMetadata);
        } else if (currentEvent === 'session_compacted') {
          // Support double-wrap: { "session_compacted", "data": { "removed_messages": 5, "summary": "..." } }
          const compactData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          handlers.onSessionCompacted?.(compactData.removed_messages ?? 0, compactData.summary ?? '');
        } else if (currentEvent === 'thinking_start') {
          // Support double-wrap format: { "thinking_start", "data": { "index": 0 } }
          if (typeof data === 'object' && data.data !== undefined) {
            handlers.onThinkingStart?.(data.data.index ?? 0);
          } else {
            handlers.onThinkingStart?.(typeof data === 'number' ? data : (data.index ?? 0));
          }
        } else if (currentEvent === 'thinking_delta') {
          // StreamingReporter sends {type, data} format: { "thinking_delta", "data": { "index": 0, "text": "..." } }
          // Also support unwrapped: { "index": 0, "text": "..." }
          let delta = '';
          if (typeof data === 'string') {
            delta = data;
          } else if (data.text !== undefined) {
            delta = data.text;
          } else if (data.thinking !== undefined) {
            delta = data.thinking;
          } else if (data.data) {
            delta = typeof data.data === 'string' ? data.data : (data.data.text ?? data.data.thinking ?? '');
          }
          if (delta) {
            thinkingDeltas.push(delta);
            handlers.onThinkingDelta?.(delta);
          }
        } else if (currentEvent === 'thinking_end') {
          // Also support double-wrap: { "thinking_end", "data": { "index": 0 } }
          if (typeof data === 'object' && data.data !== undefined) {
            handlers.onThinkingEnd?.(data.data.index ?? 0);
          } else {
            handlers.onThinkingEnd?.(typeof data === 'number' ? data : (data.index ?? 0));
          }
        } else if (currentEvent === 'text_block_start') {
          // StreamingReporter sends {type, data} format with double-wrapping:
          // { "type": "text_block_start", "data": { "index": 0 } }
          if (typeof data === 'object' && data.data !== undefined) {
            handlers.onTextBlockStart?.(data.data.index ?? 0);
          } else {
            handlers.onTextBlockStart?.(typeof data === 'number' ? data : (data.index ?? 0));
          }
        } else if (currentEvent === 'text_delta') {
          // StreamingReporter sends {type, data} double-wrapped format:
          // { "type": "text_delta", "data": { "index": 0, "text": "..." } }
          // The TurnResult summary sends unwrapped: { "index": 0, "text": "..." }
          // Support both.
          let text = '';
          if (typeof data === 'string') {
            text = data;
          } else if (data.text !== undefined) {
            text = data.text;
          } else if (data.data && typeof data.data === 'string') {
            text = data.data;
          } else if (data.data && data.data.text !== undefined) {
            text = data.data.text;
          }
          if (text) {
            textDeltas.push(text);
            enqueueTextDelta(text);
          }
        } else if (currentEvent === 'text_block_end') {
          if (typeof data === 'object' && data.data !== undefined) {
            handlers.onTextBlockEnd?.(data.data.index ?? 0);
          } else {
            handlers.onTextBlockEnd?.(typeof data === 'number' ? data : (data.index ?? 0));
          }
        } else if (currentEvent === 'thinking') {
          // Single thinking event from the agent route (full content at end of turn).
          const thinking = data.thinking ?? '';
          if (thinking) {
            thinkingDeltas.push(thinking);
            handlers.onThinkingDelta?.(thinking);
          }
        } else if (currentEvent === 'tool_use_start') {
          // Support double-wrap: { "tool_use_start", "data": { "index": 0, "id": "...", "name": "..." } }
          let idx = 0, id = '', name = '';
          if (typeof data === 'object' && data.data !== undefined) {
            idx = data.data.index ?? 0; id = data.data.id ?? ''; name = data.data.name ?? '';
          } else {
            idx = data.index ?? 0; id = data.id ?? ''; name = data.name ?? '';
          }
          handlers.onToolUseStart?.(idx, id, name);
        } else if (currentEvent === 'tool_use_input') {
          // Support double-wrap: { "tool_use_input", "data": { "index": 0, "input": "..." } }
          let idx = 0, input = '';
          if (typeof data === 'object' && data.data !== undefined) {
            idx = data.data.index ?? 0; input = data.data.input ?? '';
          } else {
            idx = data.index ?? 0; input = data.input ?? '';
          }
          handlers.onToolInputDelta?.(idx, input);
        } else if (currentEvent === 'tool_use_end') {
          // Support double-wrap: { "tool_use_end", "data": { "index": 0 } }
          let idx = 0;
          if (typeof data === 'object' && data.data !== undefined) {
            idx = data.data.index ?? 0;
          } else {
            idx = typeof data === 'number' ? data : (data.index ?? 0);
          }
          handlers.onToolUseEnd?.(idx);
        } else if (currentEvent === 'tool_result') {
          // Support double-wrap: { "tool_result", "data": { "index": 0, "tool_name": "...", "input": "...", "output": "...", "is_error": false, "duration_ms": 123 } }
          let idx = 0, toolName = '', input = '', output = '', isError = false, durationMs: number | undefined;
          if (typeof data === 'object' && data.data !== undefined) {
            idx = data.data.index ?? 0;
            toolName = data.data.tool_name ?? '';
            input = data.data.input ?? '';
            output = data.data.output ?? '';
            isError = data.data.is_error ?? false;
            durationMs = data.data.duration_ms;
          } else {
            idx = data.index ?? 0;
            toolName = data.tool_name ?? '';
            input = data.input ?? '';
            output = data.output ?? '';
            isError = data.is_error ?? false;
            durationMs = data.duration_ms;
          }
          handlers.onToolResult?.(idx, toolName, input, output, isError, durationMs);
        } else if (currentEvent === 'tool_call') {
          // Legacy tool_call event with full data (sent at turn end).
          handlers.onToolCall?.(data as AgentToolCall);
        } else if (currentEvent === 'usage') {
          // Support double-wrap: { "usage", "data": { "input_tokens": 100, ... } }
          const usageData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          latestUsage = usageData as AgentUsage;
          handlers.onUsage?.(latestUsage);
        } else if (currentEvent === 'pm_quality') {
          const q = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          latestPmQuality = q;
          handlers.onPmQuality?.(q);
        } else if (currentEvent === 'pm_stage') {
          const stageData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          handlers.onPmStage?.(stageData);
        } else if (currentEvent === 'pm_report') {
          const reportData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          if (reportData && typeof reportData === 'object' && !Array.isArray(reportData)) {
            latestPmReport = reportData as Record<string, unknown>;
          }
        } else if (currentEvent === 'text') {
          // Non-streaming full-text event from the turn-end summary.
          // Captured here so it can be passed to onStreamEnd as the authoritative final text.
          // Prefer this over textDeltas when available.
          const textVal = typeof data === 'string' ? data : (data.text ?? '');
          if (textVal) finalText = textVal;
        } else if (currentEvent === 'stream_end') {
          terminalEventSeen = true;
          // Support double-wrap: { "stream_end", "data": { "iterations": 1, ... } }
          const endData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          drainTextQueue();
          const text = resolveStreamText();
          const thinking = thinkingDeltas.join('');
          handlers.onStreamEnd?.(endData.iterations ?? 0, latestUsage, text, thinking, {
            pm_quality: latestPmQuality,
            pm_report: latestPmReport,
            streamMode: endData.streamMode ?? endData.stream_mode,
            telemetry: endData.telemetry,
          });
        } else if (currentEvent === 'error') {
          terminalEventSeen = true;
          // Support double-wrap: { "error", "data": { "error": "..." } }
          const errData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          handlers.onError?.(errData.error ?? '未知错误');
        } else if (currentEvent === 'image_context_warning') {
          const warningData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          handlers.onImageContextWarning?.({
            message:
              warningData?.message ??
              '图片解析部分失败，系统将继续基于可用信息回答。',
            detail: warningData?.detail,
            code: warningData?.code,
          });
        } else if (currentEvent === 'super_assistant_answer') {
          const answerData = (typeof data === 'object' && data.data !== undefined) ? data.data : data;
          handlers.onSuperAssistantAnswer?.(answerData as SuperAssistantAnswer);
        }
      } catch {
        // Ignore malformed SSE records and continue with later events.
      }
      if (currentEventSeq > 0) {
        lastEventSeq = Math.max(lastEventSeq, currentEventSeq);
      }
      currentEvent = '';
      currentData = '';
      currentEventSeq = 0;
    };

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        flush();
        break;
      }
      const chunk = decoder.decode(value, { stream: true });
      buffer += chunk;

      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed) {
          flush();
          continue;
        }
        if (trimmed.startsWith('event:')) {
          flush(); // Flush any pending event before starting a new one
          let evt = trimmed.slice(5).trim();
          if (evt.startsWith(':')) evt = evt.slice(1).trim();
          currentEvent = evt;
        } else if (trimmed.startsWith('id:')) {
          const seq = Number.parseInt(trimmed.slice(3).trim(), 10);
          currentEventSeq = Number.isFinite(seq) && seq > 0 ? seq : 0;
        } else if (trimmed.startsWith('data:')) {
          currentData = trimmed.slice(5).trim();
        } else if (currentData) {
          currentData += '\n' + trimmed;
        }
      }
    }
    // Flush any remaining incomplete event at stream end
    if (currentEvent || currentData) flush();
    // Safety net: some upstream/proxy paths may close SSE without explicit
    // `stream_end` / `error`. Without this, UI can stay in infinite loading.
    if (!aborted && requestSeq === _streamAgentSeq && !terminalEventSeen) {
      if (resumePersistedSuperAssistantTurn()) return;
      drainTextQueue();
      const partialText = resolveStreamText();
      const partialThinking = thinkingDeltas.join('');
      if (partialText || partialThinking) {
        handlers.onStreamEnd?.(0, latestUsage, partialText, partialThinking, {
          pm_quality: latestPmQuality,
          pm_report: latestPmReport,
        });
      } else {
        handlers.onError?.('流提前结束，未收到 stream_end');
      }
    }
  }).catch((err) => {
    stopTextFlushTimer();
    pendingTextQueue = '';
    // Only surface errors for the most recent request. If requestSeq has advanced
    // (meaning a newer request superseded this one), silently discard the error.
    if (requestSeq !== _streamAgentSeq) return;
    if (resumePersistedSuperAssistantTurn()) return;
    handlers.onError?.(err.message);
  });

  return () => {
    aborted = true;
    stopTextFlushTimer();
    pendingTextQueue = '';
    reader?.cancel();
    resumedStreamAbort?.();
    resumedStreamAbort = null;
  };
}

export type AgentSessionStreamHandlers = Parameters<typeof streamAgentSession>[2];

function applySuperAssistantEventEffects(
  effects: SuperAssistantEventEffect[],
  state: SuperAssistantEventState,
  handlers: AgentSessionStreamHandlers,
  enqueueText: (text: string) => void,
  flushText: () => void,
  setTurnId?: (turnId: string) => void,
) {
  for (const effect of effects) {
    switch (effect.type) {
      case 'route':
        if (effect.turnId?.trim()) setTurnId?.(effect.turnId.trim());
        break;
      case 'session_activated':
        handlers.onSessionActivated?.(effect.value as import('@/types').SessionMetadata);
        break;
      case 'config_hot_reload':
        handlers.onConfigHotReload?.(effect.value as import('@/types').SessionMetadata);
        break;
      case 'session_compacted':
        handlers.onSessionCompacted?.(effect.removedMessages, effect.summary);
        break;
      case 'thinking_start':
        handlers.onThinkingStart?.(effect.index);
        break;
      case 'thinking_delta':
        handlers.onThinkingDelta?.(effect.text);
        break;
      case 'commentary_delta':
        // Parent-agent drafts are process commentary, not the authoritative
        // answer. Reuse the existing collapsible reasoning surface so users
        // see progress without contaminating the final text accumulator.
        handlers.onThinkingDelta?.(effect.text);
        break;
      case 'thinking_end':
        handlers.onThinkingEnd?.(effect.index);
        break;
      case 'text_block_start':
        handlers.onTextBlockStart?.(effect.index);
        break;
      case 'text_delta':
        enqueueText(effect.text);
        break;
      case 'text_block_end':
        handlers.onTextBlockEnd?.(effect.index);
        break;
      case 'tool_start':
        handlers.onToolUseStart?.(effect.index, effect.id, effect.name);
        break;
      case 'tool_input':
        handlers.onToolInputDelta?.(effect.index, effect.input);
        break;
      case 'tool_end':
        handlers.onToolUseEnd?.(effect.index);
        break;
      case 'tool_result':
        handlers.onToolResult?.(
          effect.index,
          effect.toolName,
          effect.input,
          effect.output,
          effect.isError,
          effect.durationMs,
        );
        break;
      case 'tool_call':
        handlers.onToolCall?.(effect.value as AgentToolCall);
        break;
      case 'usage':
        handlers.onUsage?.(effect.value as AgentUsage);
        break;
      case 'pm_quality':
        handlers.onPmQuality?.(
          effect.value as Parameters<NonNullable<AgentSessionStreamHandlers['onPmQuality']>>[0],
        );
        break;
      case 'pm_report':
        break;
      case 'pm_stage':
        handlers.onPmStage?.(
          effect.value as Parameters<NonNullable<AgentSessionStreamHandlers['onPmStage']>>[0],
        );
        break;
      case 'image_context_warning':
        handlers.onImageContextWarning?.({
          message:
            typeof effect.value.message === 'string'
              ? effect.value.message
              : '图片解析部分失败，系统将继续基于可用信息回答。',
          detail: typeof effect.value.detail === 'string' ? effect.value.detail : undefined,
          code: typeof effect.value.code === 'string' ? effect.value.code : undefined,
        });
        break;
      case 'super_assistant_answer':
        handlers.onSuperAssistantAnswer?.(effect.value as SuperAssistantAnswer);
        break;
      case 'stream_end':
        flushText();
        handlers.onStreamEnd?.(
          effect.iterations,
          state.usage as AgentUsage | undefined,
          effect.fullText,
          effect.thinking,
          {
            pm_quality: state.pmQuality as Parameters<
              NonNullable<AgentSessionStreamHandlers['onPmQuality']>
            >[0],
            pm_report: state.pmReport,
            streamMode: effect.streamMode,
            telemetry: effect.telemetry as any,
          },
        );
        break;
      case 'error':
        handlers.onError?.(effect.message);
        break;
    }
  }
}

/** Replay and follow a persisted Super Assistant turn stream. */
export function streamSuperAssistantTurnEvents(
  turnId: string,
  handlers: AgentSessionStreamHandlers,
  options?: {
    afterSeq?: number;
    sessionId?: string;
    reducerState?: SuperAssistantEventState;
  },
) {
  handlers.onSuperAssistantTurnId?.(turnId);
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');
  const requestSeq = ++_streamAgentSeq;
  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';
  let currentEventSeq = 0;
  let eventState = options?.reducerState ?? createSuperAssistantEventState(options?.afterSeq);
  let lastEventSeq = eventState.lastEventId;
  let terminalEventSeen = eventState.terminal;
  let pendingText = '';
  let textFlushTimer: ReturnType<typeof setTimeout> | null = null;
  const MAX_REPLAY_BATCH_CHARS = 4096;

  const stopTextFlushTimer = () => {
    if (textFlushTimer != null) {
      clearTimeout(textFlushTimer);
      textFlushTimer = null;
    }
  };
  const flushPendingText = () => {
    stopTextFlushTimer();
    if (!pendingText || aborted || requestSeq !== _streamAgentSeq) {
      pendingText = '';
      return;
    }
    const text = pendingText;
    pendingText = '';
    handlers.onText?.(text);
  };
  const enqueueText = (text: string) => {
    if (!text) return;
    pendingText += text;
    if (
      (typeof document !== 'undefined' && document.hidden) ||
      pendingText.length >= MAX_REPLAY_BATCH_CHARS
    ) {
      flushPendingText();
      return;
    }
    if (textFlushTimer == null) {
      textFlushTimer = setTimeout(flushPendingText, 32);
    }
  };

  const flush = () => {
    if (!currentEvent || !currentData) {
      currentEvent = '';
      currentData = '';
      return;
    }
    if (requestSeq !== _streamAgentSeq || aborted) {
      stopTextFlushTimer();
      pendingText = '';
      currentEvent = '';
      currentData = '';
      currentEventSeq = 0;
      return;
    }
    try {
      const parsed = JSON.parse(currentData);
      const reduced = reduceSuperAssistantEvent(eventState, {
        id: currentEventSeq,
        event: currentEvent,
        data: parsed,
      });
      eventState = reduced.state;
      applySuperAssistantEventEffects(
        reduced.effects,
        eventState,
        handlers,
        enqueueText,
        flushPendingText,
      );
      lastEventSeq = eventState.lastEventId;
      terminalEventSeen = eventState.terminal;
    } catch {
      // Reconnect logic can recover from a malformed persisted event.
    }
    currentEvent = '';
    currentData = '';
    currentEventSeq = 0;
  };

  void (async () => {
    const maxReconnects = 6;
    let reconnects = 0;
    while (!aborted && requestSeq === _streamAgentSeq && !terminalEventSeen) {
      try {
        const params = new URLSearchParams();
        if (lastEventSeq > 0) params.set('afterSeq', String(lastEventSeq));
        if (options?.sessionId?.trim()) params.set('sessionId', options.sessionId.trim());
        const suffix = params.toString() ? `?${params.toString()}` : '';
        const response = await fetch(
          `${baseUrl}/api/v1/super-assistant/turns/${encodeURIComponent(turnId)}/events${suffix}`,
          {
            method: 'GET',
            headers: {
              ...(token ? { Authorization: `Bearer ${token}` } : {}),
              ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
            },
          },
        );
        if (!response.ok) {
          const text = await response.text();
          if (
            response.status >= 400 &&
            response.status < 500 &&
            response.status !== 404
          ) {
            handlers.onError?.(`Request failed: ${response.status} ${text}`);
            return;
          }
          throw new Error(`Request failed: ${response.status} ${text}`);
        }
        if (!response.body) throw new Error('Empty response body');

        reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = '';
        while (!aborted) {
          const { done, value } = await reader.read();
          if (done) {
            buffer += decoder.decode();
            if (buffer.trim()) {
              buffer += '\n\n';
            }
            const trailing = buffer.split('\n');
            buffer = '';
            for (const raw of trailing) {
              const trimmed = raw.trim();
              if (!trimmed) {
                flush();
              } else if (trimmed.startsWith('event:')) {
                flush();
                let evt = trimmed.slice(5).trim();
                if (evt.startsWith(':')) evt = evt.slice(1).trim();
                currentEvent = evt;
              } else if (trimmed.startsWith('id:')) {
                const seq = Number.parseInt(trimmed.slice(3).trim(), 10);
                currentEventSeq = Number.isFinite(seq) && seq > 0 ? seq : 0;
              } else if (trimmed.startsWith('data:')) {
                const line = trimmed.slice(5).trim();
                currentData = currentData ? `${currentData}\n${line}` : line;
              } else if (currentData) {
                currentData += `\n${trimmed}`;
              }
            }
            break;
          }
          buffer += decoder.decode(value, { stream: true });
          const lines = buffer.split('\n');
          buffer = lines.pop() ?? '';
          for (const raw of lines) {
            const trimmed = raw.trim();
            if (!trimmed) {
              flush();
              continue;
            }
            if (trimmed.startsWith('event:')) {
              flush();
              let evt = trimmed.slice(5).trim();
              if (evt.startsWith(':')) evt = evt.slice(1).trim();
              currentEvent = evt;
            } else if (trimmed.startsWith('id:')) {
              const seq = Number.parseInt(trimmed.slice(3).trim(), 10);
              currentEventSeq = Number.isFinite(seq) && seq > 0 ? seq : 0;
            } else if (trimmed.startsWith('data:')) {
              const line = trimmed.slice(5).trim();
              currentData = currentData ? `${currentData}\n${line}` : line;
            } else if (currentData) {
              currentData += `\n${trimmed}`;
            }
          }
        }
      } catch (error) {
        if (aborted || requestSeq !== _streamAgentSeq) return;
        if (reconnects >= maxReconnects) {
          stopTextFlushTimer();
          pendingText = '';
          handlers.onError?.(error instanceof Error ? error.message : String(error));
          return;
        }
      }

      if (aborted || requestSeq !== _streamAgentSeq || terminalEventSeen) return;
      if (reconnects >= maxReconnects) {
        stopTextFlushTimer();
        pendingText = '';
        handlers.onError?.('流提前结束，重连后仍未收到 stream_end');
        return;
      }
      reconnects += 1;
      const delayMs = Math.min(5_000, 400 * 2 ** (reconnects - 1));
      await new Promise((resolve) => setTimeout(resolve, delayMs));
    }
  })();

  return () => {
    aborted = true;
    stopTextFlushTimer();
    pendingText = '';
    reader?.cancel();
  };
}

/** SSE streaming for a Super Adversarial run. */
export function streamChatAdversarialRunEvents(
  runId: string,
  handlers: {
    onEvent?: (event: ChatAdversarialStreamEvent) => void;
    onError?: (error: string) => void;
    onEnd?: () => void;
  },
  options?: {
    afterSeq?: number;
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');
  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';
  const params = new URLSearchParams();
  if (options?.afterSeq && options.afterSeq > 0) {
    params.set('after_seq', String(options.afterSeq));
  }
  const suffix = params.toString() ? `?${params.toString()}` : '';

  const flush = () => {
    if (!currentEvent || !currentData) {
      currentEvent = '';
      currentData = '';
      return;
    }
    if (aborted) {
      currentEvent = '';
      currentData = '';
      return;
    }
    try {
      if (currentEvent === 'adversarial_event') {
        handlers.onEvent?.(JSON.parse(currentData) as ChatAdversarialStreamEvent);
      }
    } catch {
      handlers.onError?.('Failed to parse Super Debate event stream');
    }
    currentEvent = '';
    currentData = '';
  };

  fetch(
    `${baseUrl}/api/v1/agent/chat-adversarial-runs/${encodeURIComponent(runId)}/events${suffix}`,
    {
      method: 'GET',
      headers: {
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
      },
    },
  )
    .then(async (response) => {
      if (!response.ok) {
        const text = await response.text();
        handlers.onError?.(`Request failed: ${response.status} ${text}`);
        return;
      }
      const stream = response.body;
      if (!stream) {
        handlers.onError?.('Empty response body');
        return;
      }

      reader = stream.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      while (true) {
        if (aborted) break;
        const { done, value } = await reader.read();
        if (done) {
          flush();
          break;
        }
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() ?? '';
        for (const raw of lines) {
          const trimmed = raw.trim();
          if (!trimmed) {
            flush();
            continue;
          }
          if (trimmed.startsWith('event:')) {
            flush();
            let evt = trimmed.slice(5).trim();
            if (evt.startsWith(':')) evt = evt.slice(1).trim();
            currentEvent = evt;
          } else if (trimmed.startsWith('data:')) {
            const dataLine = trimmed.slice(5).trim();
            currentData = currentData ? `${currentData}\n${dataLine}` : dataLine;
          } else if (currentData) {
            currentData += `\n${trimmed}`;
          }
        }
      }
      if (!aborted) {
        handlers.onEnd?.();
      }
    })
    .catch((error) => {
      if (aborted) return;
      handlers.onError?.(error instanceof Error ? error.message : String(error));
    });

  return () => {
    aborted = true;
    reader?.cancel();
  };
}
