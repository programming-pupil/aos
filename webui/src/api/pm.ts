import { client, fastClient } from './client';
import type { ChatMessage } from '@/types';

export interface PmResearchTaskStartResponse {
  task_id?: string;
  taskId?: string;
  status: string;
}

export interface PmTaskImageInput {
  url: string;
  fileId?: string;
  mediaType?: string;
  name?: string;
  sizeBytes?: number;
}

export interface PmTaskDocumentInput {
  url: string;
  fileId?: string;
  mediaType?: string;
  name?: string;
  sizeBytes?: number;
}

export interface PmMaterialModelOption {
  model: string;
}

export type PmMaterialAssetType = 'text' | 'image' | 'music' | 'ppt';

export interface PmMissionSummaryResponse {
  totalMissions: number;
  enabledMissions: number;
  disabledMissions: number;
  queuedRuns: number;
  runningRuns: number;
  cancellingRuns: number;
  completedRuns30d: number;
  failedRuns30d: number;
  cancelledRuns30d: number;
  successRate30d: number;
  avgElapsedMs30d?: number | null;
  latestRunAt?: string | null;
}

export interface PmMaterialSummaryResponse {
  totalJobs: number;
  totalThreads: number;
  runningJobs: number;
  completedJobs30d: number;
  failedJobs30d: number;
  successRate30d: number;
  textJobs30d: number;
  imageJobs30d: number;
  musicJobs30d: number;
  pptJobs30d: number;
  assetCount30d: number;
  versionedJobs: number;
  latestJobAt?: string | null;
}

export interface PmResearchTaskControlResponse {
  task_id?: string;
  taskId?: string;
  status: string;
  cancel_requested?: boolean;
  cancelRequested?: boolean;
  restarted?: boolean;
}

export interface PmResearchTaskStatusResponse {
  taskId: string;
  sessionId: string;
  status: string;
  stage?: string;
  attempt?: number;
  message?: string;
  elapsedMs: number;
  stageElapsedMs?: number;
  detail?: Record<string, unknown>;
  response?: Record<string, unknown>;
  error?: string;
  cancelRequested: boolean;
}

export interface PmResearchTaskEvent {
  task_id: string;
  session_id: string;
  status: string;
  stage?: string;
  attempt?: number;
  message?: string;
  elapsed_ms: number;
  stage_elapsed_ms?: number;
  detail?: Record<string, unknown>;
  response?: Record<string, unknown>;
  error?: string;
}

export interface PmSubtaskRuntimeRow {
  id: number;
  run_id: string;
  task_id?: string;
  subtask_key: string;
  subtask_id?: string;
  title: string;
  goal?: string;
  deliverable?: string;
  required_evidence_type?: string;
  priority: string;
  status: string;
  probe_candidate_count: number;
  probe_completed_count: number;
  citation_count: number;
  domain_count: number;
  tool_call_count: number;
  quality_score?: number;
  error_code?: string;
  error_message?: string;
  detail?: Record<string, unknown>;
  started_at?: string;
  ended_at?: string;
  updated_at?: string;
}

export interface PmSubtaskAttemptRow {
  id: number;
  subtask_run_id: number;
  run_id: string;
  subtask_key: string;
  attempt_no: number;
  attempt_key: string;
  variant?: string;
  route_key?: string;
  route_channel?: string;
  status: string;
  elapsed_ms?: number;
  citation_count: number;
  domain_count: number;
  tool_call_count: number;
  quality_score?: number;
  error_code?: string;
  error_message?: string;
  detail?: Record<string, unknown>;
  started_at?: string;
  ended_at?: string;
  updated_at?: string;
}

export interface PmStrategyRecordRequest {
  route: string;
  channel?: string;
  variant?: string;
  passed: boolean;
  citation_count?: number;
  domain_count?: number;
  tool_call_count?: number;
  retrieve_duration_ms?: number;
  estimated_cost_usd?: number;
}

export interface PmStrategyLeaderboardItem {
  route: string;
  channel?: string;
  runCount: number;
  successRate: number;
  avgQuality: number;
  avgCost: number;
  avgRetrieveDurationMs: number;
  score: number;
  lastRunAt?: string;
}

export interface PmStrategyLeaderboardResponse {
  rows: PmStrategyLeaderboardItem[];
}

export interface PmBudgetProfileRow {
  profileKey: string;
  displayName?: string;
  enabled: boolean;
  isDefault: boolean;
  priority: number;
  pipelineTimeoutSecs: number;
  maxAttempts: number;
  retrieveMaxToolCalls: number;
  maxCallsPerSource: number;
  sourceSlotSearchSecs: number;
  sourceSlotBrowserSecs: number;
  sourceSlotApiFetchSecs: number;
  preflightModelTimeoutSecs: number;
  preflightProbeTimeoutSecs: number;
  preflightOverallTimeoutSecs: number;
  retryStepBudgetSecs: number;
  retryTotalBudgetSecs: number;
  constraintsJson?: Record<string, unknown>;
  updatedAt?: string;
}

export interface PmBudgetProfilesResponse {
  rows: PmBudgetProfileRow[];
  total: number;
}

export interface PmBudgetProfileActivateResponse {
  ok: boolean;
  activeProfile: string;
  budgetSnapshot: {
    pipelineTimeoutSecs: number;
    maxAttempts: number;
    retrieveMaxToolCalls: number;
    maxCallsPerSource: number;
    sourceSlotSearchSecs: number;
    sourceSlotBrowserSecs: number;
    sourceSlotApiFetchSecs: number;
    retryStepBudgetSecs: number;
    retryTotalBudgetSecs: number;
  };
}

export interface PmSloWindowSummary {
  windowDays: number;
  totalRuns: number;
  completedRuns: number;
  failedRuns: number;
  cancelledRuns: number;
  successRate: number;
  qualitySampleRuns?: number;
  qualitySampleCoverage?: number;
  qualityPassRate: number;
  terminalRate?: number;
  answerDeliveryRate?: number;
  conclusionDeliveryRate: number;
  latencySampleCount?: number;
  latencyP50Ms?: number;
  latencyP95Ms?: number;
  latencyP99Ms?: number;
}

export interface PmSloSummaryResponse {
  rows: PmSloWindowSummary[];
  generatedAt: string;
}

export interface PmFailureTaxonomyRow {
  errorCode: string;
  runCount: number;
  failedCount: number;
  completedCount: number;
  objectCount?: number;
  runFailureCount?: number;
  subtaskFailureCount?: number;
  toolFailureCount?: number;
  avgElapsedMs?: number;
  lastSeenAt?: string;
}

export interface PmFailureTaxonomyResponse {
  rows: PmFailureTaxonomyRow[];
  days: number;
  total: number;
}

export interface PmProviderHealthRow {
  providerKey: string;
  channel: string;
  runCount: number;
  successCount: number;
  failureCount: number;
  avgLatencyMs?: number;
  lastErrorCode?: string;
  lastStatus: string;
  lastCheckedAt?: string;
}

export interface PmProviderHealthResponse {
  rows: PmProviderHealthRow[];
  total: number;
  scope?: 'lifetime_aggregate';
  generatedAt?: string;
}

export interface PmRouteLearningFeatureRow {
  route: string;
  channel?: string;
  totalRuns: number;
  successRuns: number;
  failedRuns: number;
  emaQuality: number;
  emaLatencyMs: number;
  emaCostUsd: number;
  emaSuccessRate: number;
  lastRunAt?: string;
}

export interface PmRouteLearningFeaturesResponse {
  rows: PmRouteLearningFeatureRow[];
  total: number;
  page?: number;
  perPage?: number;
}

export interface PmQualityGateWindowSummary {
  windowDays: number;
  totalRuns: number;
  passedRuns: number;
  passRate: number;
  avgQualityScore: number;
  avgTriadCoverage: number;
  claimAlignmentRate: number;
  conflictAdjudicatedRate: number;
  avgToolCallCount: number;
  avgCitationCount: number;
  avgDomainCount: number;
}

export interface PmQualityGateSummaryResponse {
  rows: PmQualityGateWindowSummary[];
  generatedAt: string;
}

export interface PmKnowledgeCoverageWarningRow {
  runId?: string;
  taskId?: string;
  coverageRatio: number;
  plannedSubtasks: number;
  executedSubtasks: number;
  queuedSubtasks: number;
  subtaskGapCount: number;
  dimensionGapCount: number;
  message?: string;
  createdAt?: string;
  payload?: Record<string, unknown>;
}

export interface PmKnowledgeCoverageWarningsResponse {
  days: number;
  rows: PmKnowledgeCoverageWarningRow[];
  total: number;
  page?: number;
  perPage?: number;
  summary: {
    warningCount: number;
    avgCoverageRatio: number;
    minCoverageRatio: number;
    maxQueuedSubtasks: number;
    avgSubtaskGapCount: number;
    avgDimensionGapCount: number;
  };
}

export interface PmRuntimeInsightsSummary {
  totalRuns: number;
  queuedRuns: number;
  runningRuns: number;
  completedRuns: number;
  failedRuns: number;
  cancelledRuns: number;
  terminalRuns?: number;
  terminalRate?: number;
  answerDeliveryRate?: number;
  retriedRuns: number;
  recoveredRuns: number;
  retryRecoveryRate: number;
  firstPassSuccessRate: number;
  failureRate?: number;
  manualInterruptionRate?: number;
  currentQueuedTasks: number;
  currentRunningTasks: number;
  currentQueuedSubtasks: number;
  currentRunningSubtasks: number;
  retryRepairAttempts: number;
  sourceQuotaExhaustedAttempts: number;
  sourceQuotaExhaustedRate: number;
  degradedSynthesisRate?: number;
  derived?: boolean;
}

export interface PmRuntimeQueueHealth {
  queuedCount?: number;
  runningCount?: number;
  oldestQueuedTaskAgeSecs?: number | null;
  longestRunningTaskAgeSecs?: number | null;
  staleRunningTasks: number;
  avgQueueWaitSecs?: number | null;
  longestRunningHeartbeatAgeSecs?: number | null;
  queuedTasks?: number;
  runningTasks?: number;
  queuedRuns?: number;
  runningRuns?: number;
  queuedSubtasks?: number;
  runningSubtasks?: number;
  oldestQueuedObject?: {
    objectType: 'task' | 'run' | 'subtask';
    objectId: string;
    title: string;
    createdAt: string;
    ageSecs: number;
  } | null;
}

export interface PmRuntimeCostByModelRow {
  model: string;
  provider: string;
  requestCount: number;
  totalTokens: number;
  estimatedCostUsd: number;
  pricingSource?: 'built_in' | 'custom' | 'unknown';
  usageRecordCount?: number;
}

export interface PmRuntimeCostSummary {
  requestCount: number;
  totalTokens: number;
  inputTokens: number;
  outputTokens: number;
  estimatedCostUsd: number;
  avgCostPerRunUsd: number | null;
  avgCostPerSuccessfulDeliveryUsd?: number | null;
  usageRecordCount?: number;
  pricedUsageRecordCount?: number;
  unpricedUsageRecordCount?: number;
  pricingCoverage?: number;
  costComplete?: boolean;
  costSampleRunCount?: number;
  costRunCoverage?: number;
  byModel: PmRuntimeCostByModelRow[];
  derivedFromTokenUsage?: boolean;
}

export interface PmDeepResearchSummary {
  eventCount: number;
  runCount?: number;
  scoreSampleCount?: number;
  scoreSampleCoverage?: number;
  finalizedCount: number;
  degradedSynthesisCount: number;
  followupPlannedCount: number;
  avgDecisionReadiness: number;
  avgActionability: number;
  avgFirstPartyAlignment: number;
  avgEvidenceCoverage: number;
}

export interface PmFailureDrilldownRow {
  bucket: string;
  count: number;
}

export interface PmUserOutcomeSummary {
  retryRate: number;
  manualInterruptionRate: number;
  followupRepairCount: number;
  lowQualitySignalSource?: string;
}

export interface PmRuntimeInsightsDailyRunRow {
  date: string;
  totalRuns: number;
  completedRuns: number;
  failedRuns: number;
  cancelledRuns: number;
  retriedRuns: number;
}

export interface PmRuntimeInsightsDailySourceQuotaRow {
  date: string;
  retryRepairAttempts: number;
  sourceQuotaExhaustedAttempts: number;
  sourceQuotaExhaustedRate: number;
}

export interface PmRuntimeInsightsResponse {
  days: number;
  generatedAt?: string;
  summary: PmRuntimeInsightsSummary;
  queueHealth?: PmRuntimeQueueHealth;
  cost?: PmRuntimeCostSummary;
  deepResearch?: PmDeepResearchSummary;
  failureDrilldown?: PmFailureDrilldownRow[];
  userOutcome?: PmUserOutcomeSummary;
  dailyRuns: PmRuntimeInsightsDailyRunRow[];
  dailySourceQuota: PmRuntimeInsightsDailySourceQuotaRow[];
}

export type PmSearchProviderType =
  | 'brave'
  | 'tavily'
  | 'serper'
  | 'exa'
  | 'searxng'
  | 'generic_json'
  | 'internal_http';

export interface PmSearchProviderTemplate {
  providerType: PmSearchProviderType;
  label: string;
  defaultBaseUrl: string;
  defaultMethod: 'GET' | 'POST' | 'PUT' | string;
}

export interface PmSearchProviderRecord {
  id: string;
  name: string;
  providerType: PmSearchProviderType | string;
  enabled: boolean;
  priority: number;
  baseUrl?: string | null;
  method: string;
  authType: string;
  authSecretRef?: string | null;
  hasSecret: boolean;
  keyHint?: string | null;
  headersJson?: Record<string, unknown> | null;
  queryTemplateJson?: Record<string, unknown> | null;
  responseMappingJson?: Record<string, unknown> | null;
  timeoutSecs: number;
  maxResults: number;
  fetchContentEnabled: boolean;
  contentExtractMode: string;
  domainAllowlistJson?: unknown;
  domainBlocklistJson?: unknown;
  rateLimitJson?: unknown;
  healthStatus: string;
  lastError?: string | null;
  createdBy?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface PmSearchProviderListResponse {
  items: PmSearchProviderRecord[];
  total: number;
  templates: PmSearchProviderTemplate[];
}

export interface PmSearchProviderPayload {
  name?: string;
  providerType?: PmSearchProviderType | string;
  enabled?: boolean;
  priority?: number;
  baseUrl?: string;
  method?: string;
  authType?: string;
  authSecret?: string;
  authSecretRef?: string;
  headersJson?: Record<string, unknown>;
  queryTemplateJson?: Record<string, unknown>;
  responseMappingJson?: Record<string, unknown>;
  timeoutSecs?: number;
  maxResults?: number;
  fetchContentEnabled?: boolean;
  contentExtractMode?: string;
  domainAllowlistJson?: unknown;
  domainBlocklistJson?: unknown;
  rateLimitJson?: unknown;
}

export interface PmSearchProviderTestResponse {
  ok: boolean;
  latencyMs: number;
  resultCount: number;
  error?: string | null;
  providerTrace: string[];
}

export interface PmSearchLayerStatus {
  available: boolean;
  status: string;
  detail: string;
}

export interface PmSearchProviderHealth {
  id: string;
  name: string;
  providerType: string;
  enabled: boolean;
  priority: number;
  healthStatus: string;
  hasSecret: boolean;
  lastError?: string | null;
}

export interface PmSearchLayerAvailability {
  layer: string;
  key: string;
  adapter: string;
  label: string;
  available: boolean;
  status: string;
  detail: string;
}

export interface PmSearchOrchestratorSnapshot {
  orchestrator: string;
  fallbackOrder: string[];
  adapters: string[];
  layers: PmSearchLayerAvailability[];
  effectiveOrder: string[];
  degradedReason?: string | null;
}

export interface PmSearchDoctorResponse {
  orchestrator?: PmSearchOrchestratorSnapshot;
  builtinWebSearch: PmSearchLayerStatus;
  nativeSearch: PmSearchLayerStatus;
  mcpSearch: PmSearchLayerStatus;
  configuredProviders: PmSearchProviderHealth[];
  ragLocal: PmSearchLayerStatus;
  effectiveOrder: string[];
  degradedReason?: string | null;
}

export interface PmSearchQueryResponse {
  ok: boolean;
  output?: Record<string, unknown> | null;
  error?: string | null;
}

// ---- PM (Product/Marketing Copilot) ----
export const pmApi = {
  listSearchProviders: () =>
    client.get<PmSearchProviderListResponse>('/pm/search/providers').then((r) => r.data),
  createSearchProvider: (data: PmSearchProviderPayload) =>
    client.post<PmSearchProviderRecord>('/pm/search/providers', data).then((r) => r.data),
  updateSearchProvider: (id: string, data: PmSearchProviderPayload) =>
    client.patch<PmSearchProviderRecord>(`/pm/search/providers/${encodeURIComponent(id)}`, data).then((r) => r.data),
  deleteSearchProvider: (id: string) =>
    client.delete<{ deleted: boolean }>(`/pm/search/providers/${encodeURIComponent(id)}`).then((r) => r.data),
  testSearchProvider: (id: string) =>
    client.post<PmSearchProviderTestResponse>(`/pm/search/providers/${encodeURIComponent(id)}/test`).then((r) => r.data),
  reorderSearchProviders: (providerIds: string[]) =>
    client.post<{ ok: boolean }>('/pm/search/providers/reorder', { providerIds }).then((r) => r.data),
  getSearchDoctor: () =>
    client.get<PmSearchDoctorResponse>('/pm/search/doctor').then((r) => r.data),
  getSearchCapabilities: () =>
    client.get<PmSearchDoctorResponse>('/pm/search/capabilities').then((r) => r.data),
  querySearch: (data: { query: string; providerId?: string; maxResults?: number }) =>
    client.post<PmSearchQueryResponse>('/pm/search/query', data).then((r) => r.data),
  chat: (data: { model?: string; messages: ChatMessage[] }) =>
    client.post<{
      answer: string;
      usage?: {
        inputTokens: number;
        outputTokens: number;
        totalTokens: number;
        estimatedCostUsd: number;
        model: string;
      };
      appliedRules?: Array<{
        ruleKey: string;
        ruleName: string;
        detail: string;
      }>;
    }>('/pm/chat', data).then((r) => r.data),
  listChatSessions: (params?: { page?: number; per_page?: number; search?: string }) =>
    client.get<{ items: Array<{
      id: number;
      title: string;
      createdBy?: string | null;
      lastMessageAt: string;
      pinnedAt?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/chat/sessions', { params }).then((r) => r.data),
  createChatSession: (data?: { title?: string }) =>
    client.post<{
      id: number;
      title: string;
      createdBy?: string | null;
      lastMessageAt: string;
      createdAt: string;
      updatedAt: string;
    }>('/pm/chat/sessions', data ?? {}).then((r) => r.data),
  updateChatSession: (id: number, data: { title?: string; pinned?: boolean }) =>
    client.patch(`/pm/chat/sessions/${id}`, data).then((r) => r.data),
  pinChatSession: (id: number) => client.post(`/pm/chat/sessions/${id}/pin`).then((r) => r.data),
  unpinChatSession: (id: number) => client.post(`/pm/chat/sessions/${id}/unpin`).then((r) => r.data),
  deleteChatSession: (id: number) => client.delete(`/pm/chat/sessions/${id}`).then((r) => r.data),
  listChatMessages: (sessionId: number, params?: { page?: number; per_page?: number }) =>
    client.get<{ items: Array<{
      id: number;
      role: string;
      content: string;
      usage?: {
        inputTokens: number;
        outputTokens: number;
        totalTokens: number;
        estimatedCostUsd: number;
        model: string;
      } | null;
      appliedRules?: Array<{
        ruleKey: string;
        ruleName: string;
        detail: string;
      }>;
      createdAt: string;
    }>; total: number }>(`/pm/chat/sessions/${sessionId}/messages`, { params }).then((r) => r.data),
  askChatSession: (sessionId: number, data: { content: string; model?: string }) =>
    client.post<{
      id: number;
      role: string;
      content: string;
      usage?: {
        inputTokens: number;
        outputTokens: number;
        totalTokens: number;
        estimatedCostUsd: number;
        model: string;
      } | null;
      appliedRules?: Array<{
        ruleKey: string;
        ruleName: string;
        detail: string;
      }>;
      createdAt: string;
    }>(`/pm/chat/sessions/${sessionId}`, data).then((r) => r.data),

  listCountries: (params?: { page?: number; per_page?: number }) =>
    client.get<{ items: Array<{
      id: number;
      countryCode: string;
      countryName: string;
      timezone?: string | null;
      enabled: boolean;
      priority: number;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/countries', { params }).then((r) => r.data),
  createCountry: (data: {
    countryCode: string;
    countryName: string;
    timezone?: string;
    enabled?: boolean;
    priority?: number;
  }) => client.post('/pm/countries', data).then((r) => r.data),
  updateCountry: (id: number, data: Partial<{
    countryName: string;
    timezone: string;
    enabled: boolean;
    priority: number;
  }>) => client.patch(`/pm/countries/${id}`, data).then((r) => r.data),
  deleteCountry: (id: number) => client.delete(`/pm/countries/${id}`).then((r) => r.data),
  bootstrapDefaultCountries: () =>
    client
      .post<{ inserted: number; skipped: number }>('/pm/countries/bootstrap-defaults')
      .then((r) => r.data),

  listSources: (params?: { page?: number; per_page?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      countryCode: string;
      sourceScope?: string;
      templateKey?: string | null;
      sourceType: string;
      sourceName: string;
      baseUrl?: string | null;
      config?: Record<string, unknown> | null;
      proxyPoolIds?: number[];
      enabled: boolean;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/sources', { params }).then((r) => r.data),
  createSource: (data: {
    countryCode: string;
    sourceScope?: string;
    templateKey?: string | null;
    sourceType: string;
    sourceName: string;
    baseUrl?: string;
    config?: Record<string, unknown>;
    proxyPoolIds?: number[];
    enabled?: boolean;
  }) => client.post('/pm/sources', data).then((r) => r.data),
  updateSource: (id: number, data: Partial<{
    countryCode: string;
    sourceScope: string;
    templateKey: string | null;
    sourceType: string;
    sourceName: string;
    baseUrl: string;
    config: Record<string, unknown>;
    proxyPoolIds: number[];
    enabled: boolean;
  }>) => client.patch(`/pm/sources/${id}`, data).then((r) => r.data),
  deleteSource: (id: number) => client.delete(`/pm/sources/${id}`).then((r) => r.data),
  importSourceTemplates: (data?: { countryCodes?: string[] }) =>
    client
      .post<{ inserted: number; skipped: number }>('/pm/sources/import-templates', data ?? {})
      .then((r) => r.data),
  listMissions: (params?: { page?: number; per_page?: number; country_code?: string; enabled?: boolean }) =>
    client.get<{ items: Array<{
      id: number;
      missionName: string;
      intent: string;
      countryCode: string;
      scheduleCron?: string | null;
      lookbackDays: number;
      maxSources: number;
      maxSignalsPerSource: number;
      autoDiscovery: boolean;
      enabled: boolean;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/missions', { params }).then((r) => r.data),
  getMissionSummary: () =>
    client.get<PmMissionSummaryResponse>('/pm/missions/summary').then((r) => r.data),
  createMission: (data: {
    missionName: string;
    intent: string;
    countryCode?: string;
    scheduleCron?: string;
    lookbackDays?: number;
    maxSources?: number;
    maxSignalsPerSource?: number;
    autoDiscovery?: boolean;
    enabled?: boolean;
  }) => client.post('/pm/missions', data).then((r) => r.data),
  updateMission: (id: number, data: Partial<{
    missionName: string;
    intent: string;
    countryCode: string;
    scheduleCron: string;
    lookbackDays: number;
    maxSources: number;
    maxSignalsPerSource: number;
    autoDiscovery: boolean;
    enabled: boolean;
  }>) => client.patch(`/pm/missions/${id}`, data).then((r) => r.data),
  deleteMission: (id: number) => client.delete(`/pm/missions/${id}`).then((r) => r.data),
  runMissionNow: (id: number) =>
    client.post<{
      missionId: number;
      taskId: string;
      status: string;
    }>(`/pm/missions/${id}/run-now`, {}).then((r) => r.data),
  listMissionTaskRuns: (
    id: number,
    params?: { page?: number; per_page?: number; status?: string },
  ) =>
    client.get<{ items: Array<{
      taskId: string;
      status: string;
      stage?: string | null;
      attempt?: number | null;
      elapsedMs: number;
      stageElapsedMs?: number | null;
      errorMessage?: string | null;
      detail?: Record<string, unknown> | null;
      response?: Record<string, unknown> | null;
      createdAt: string;
      updatedAt: string;
      completedAt?: string | null;
    }>; total: number }>(`/pm/missions/${id}/task-runs`, { params }).then((r) => r.data),
  previewMissionCron: (params: { scheduleCron: string; count?: number }) =>
    client.get<{
      scheduleCron: string;
      normalizedCron: string;
      nextRuns: string[];
    }>('/pm/cron/preview', { params }).then((r) => r.data),
  listMaterialJobs: (params?: {
    page?: number;
    per_page?: number;
    mission_run_id?: number;
    thread_id?: number;
    asset_type?: PmMaterialAssetType;
    status?: string;
  }) =>
    client.get<{ items: Array<{
      id: number;
      missionRunId?: number | null;
      threadId?: number | null;
      parentJobId?: number | null;
      iterationNo: number;
      promptText: string;
      model?: string | null;
      assetType: string;
      status: string;
      resultCount: number;
      errorMessage?: string | null;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/material-jobs', { params }).then((r) => r.data),
  listMaterialThreads: (params?: {
    page?: number;
    per_page?: number;
    mission_run_id?: number;
    thread_id?: number;
    asset_type?: PmMaterialAssetType;
    status?: string;
  }) =>
    client.get<{ items: Array<{
      threadId: number;
      latestJobId: number;
      missionRunId?: number | null;
      versionCount: number;
      latestIterationNo: number;
      promptText: string;
      model?: string | null;
      assetType: string;
      status: string;
      resultCount: number;
      errorMessage?: string | null;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/material-threads', { params }).then((r) => r.data),
  listMaterialModels: (params: {
    assetType?: PmMaterialAssetType;
    workflowStage?: string;
  }) =>
    client.get<{ items: PmMaterialModelOption[] }>('/pm/material-models', { params }).then((r) => r.data),
  getMaterialSummary: () =>
    client.get<PmMaterialSummaryResponse>('/pm/material-jobs/summary').then((r) => r.data),
  createMaterialJob: (data: {
    missionRunId?: number;
    threadId?: number;
    parentJobId?: number;
    continueFromAssetId?: number;
    promptText: string;
    model?: string;
    assetType?: PmMaterialAssetType;
    workflowStage?: string;
    workflowPayload?: Record<string, unknown>;
    referenceImages?: PmTaskImageInput[];
  }) => client.post('/pm/material-jobs', data).then((r) => r.data),
  deleteMaterialJob: (id: number) => client.delete(`/pm/material-jobs/${id}`).then((r) => r.data),
  deleteMaterialThread: (id: number) => client.delete(`/pm/material-threads/${id}`).then((r) => r.data),
  listMaterialAssets: (id: number) =>
    client.get<{ items: Array<{
      id: number;
      jobId: number;
      assetType: string;
      url?: string | null;
      contentText?: string | null;
      meta: Record<string, unknown>;
      createdAt: string;
    }>; total: number }>(`/pm/material-jobs/${id}/assets`).then((r) => r.data),
  exportMaterialAsset: (id: number, format: 'pdf' | 'pptx') =>
    client.post<{
      assetId: number;
      format: 'pdf' | 'pptx';
      url: string;
    }>(`/pm/material-assets/${id}/export`, null, { params: { format } }).then((r) => r.data),
  probeSource: (data: {
    sourceType?: string;
    sourceName?: string;
    baseUrl: string;
    config?: Record<string, unknown>;
    proxyPoolIds?: number[];
  }) =>
    client.post<{ ok: boolean; fetchedCount: number; sampleCount: number; samples: Array<{
      title?: string | null;
      contentPreview: string;
      url?: string | null;
      author?: string | null;
    }>; warnings: string[] }>('/pm/sources/probe', data).then((r) => r.data),
  probeSourceSmart: (data: {
    sourceType?: string;
    sourceName?: string;
    baseUrl: string;
    config?: Record<string, unknown>;
    proxyPoolIds?: number[];
  }) =>
    client.post<{
      ok: boolean;
      detectedMode: string;
      confidence: number;
      fetchedCount: number;
      sampleCount: number;
      samples: Array<{
        title?: string | null;
        contentPreview: string;
        url?: string | null;
        author?: string | null;
      }>;
      fieldCoverage: Record<string, unknown>;
      suggestedConfig: Record<string, unknown>;
      warnings: string[];
    }>('/pm/sources/probe-smart', data).then((r) => r.data),
  listProxyPools: (params?: { page?: number; per_page?: number }) =>
    client.get<{ items: Array<{
      id: number;
      poolName: string;
      description?: string | null;
      strategy: 'priority' | 'round_robin' | 'random';
      enabled: boolean;
      priority: number;
      endpointCount: number;
      healthyEndpointCount: number;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/proxy-pools', { params }).then((r) => r.data),
  createProxyPool: (data: {
    poolName: string;
    description?: string;
    strategy?: 'priority' | 'round_robin' | 'random';
    enabled?: boolean;
    priority?: number;
  }) => client.post('/pm/proxy-pools', data).then((r) => r.data),
  updateProxyPool: (id: number, data: Partial<{
    poolName: string;
    description: string;
    strategy: 'priority' | 'round_robin' | 'random';
    enabled: boolean;
    priority: number;
  }>) => client.patch(`/pm/proxy-pools/${id}`, data).then((r) => r.data),
  deleteProxyPool: (id: number) => client.delete(`/pm/proxy-pools/${id}`).then((r) => r.data),
  listProxyPoolEndpoints: (poolId: number, params?: { page?: number; per_page?: number }) =>
    client.get<{ items: Array<{
      id: number;
      poolId: number;
      endpointUrl: string;
      username?: string | null;
      hasPassword: boolean;
      priority: number;
      weight: number;
      maxFailures: number;
      coolDownSeconds: number;
      enabled: boolean;
      consecutiveFailures: number;
      cooldownUntil?: string | null;
      lastSuccessAt?: string | null;
      lastFailureAt?: string | null;
      lastError?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>(`/pm/proxy-pools/${poolId}/endpoints`, { params }).then((r) => r.data),
  createProxyPoolEndpoint: (poolId: number, data: {
    endpointUrl: string;
    username?: string;
    password?: string;
    priority?: number;
    weight?: number;
    maxFailures?: number;
    coolDownSeconds?: number;
    enabled?: boolean;
  }) => client.post(`/pm/proxy-pools/${poolId}/endpoints`, data).then((r) => r.data),
  updateProxyPoolEndpoint: (id: number, data: Partial<{
    endpointUrl: string;
    username: string;
    password: string;
    priority: number;
    weight: number;
    maxFailures: number;
    coolDownSeconds: number;
    enabled: boolean;
  }>) => client.patch(`/pm/proxy-endpoints/${id}`, data).then((r) => r.data),
  probeProxyPoolEndpoint: (id: number, data?: {
    targetUrl?: string;
    timeoutSeconds?: number;
  }) =>
    client.post<{
      ok: boolean;
      endpointId: number;
      endpointUrl: string;
      targetUrl: string;
      statusCode?: number | null;
      latencyMs: number;
      exitIp?: string | null;
      bodyPreview?: string | null;
      error?: string | null;
      testedAt: string;
    }>(`/pm/proxy-endpoints/${id}/probe`, data ?? {}).then((r) => r.data),
  deleteProxyPoolEndpoint: (id: number) => client.delete(`/pm/proxy-endpoints/${id}`).then((r) => r.data),
  getPmPolicySettings: () =>
    client.get<{
      tenantId: string;
      allowedDomains: unknown[];
      blockedDomains: unknown[];
      blockedUrlPatterns: unknown[];
      allowExternalDomains: boolean;
      piiMaskingEnabled: boolean;
      updatedBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>('/pm/policy-settings').then((r) => r.data),
  updatePmPolicySettings: (data: {
    allowedDomains?: unknown[];
    blockedDomains?: unknown[];
    blockedUrlPatterns?: unknown[];
    allowExternalDomains?: boolean;
    piiMaskingEnabled?: boolean;
  }) => client.patch('/pm/policy-settings', data).then((r) => r.data),
  getPmQualityGates: () =>
    client.get<{
      tenantId: string;
      enabled: boolean;
      blockOnFail: boolean;
      minQualityScore: number;
      maxErrorCount: number;
      minContentCoverage: number;
      minTitleCoverage: number;
      updatedBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>('/pm/quality-gates').then((r) => r.data),
  updatePmQualityGates: (data: {
    enabled?: boolean;
    blockOnFail?: boolean;
    minQualityScore?: number;
    maxErrorCount?: number;
    minContentCoverage?: number;
    minTitleCoverage?: number;
  }) => client.patch('/pm/quality-gates', data).then((r) => r.data),
  listPmSloSnapshots: (params?: { page?: number; per_page?: number }) =>
    client.get<{ items: Array<{
      snapshotDate: string;
      collectionSuccessRate: number;
      collectionAvgLatencyMs: number;
      collectionErrorRate: number;
      modelFailureRate: number;
      signalFreshnessHours: number;
      metrics: Record<string, unknown>;
      createdAt: string;
    }>; total: number }>('/pm/slo-snapshots', { params }).then((r) => r.data),
  listConnectors: (params?: { page?: number; per_page?: number }) =>
    client.get<{ items: Array<{
      id: number;
      connectorKey: string;
      displayName: string;
      connectorType: string;
      mode: string;
      baseUrl?: string | null;
      apiPriority: number;
      config?: Record<string, unknown> | null;
      enabled: boolean;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/connectors', { params }).then((r) => r.data),
  listConnectorTemplates: () =>
    client.get<{ items: Array<{
      id: number;
      templateKey: string;
      displayName: string;
      description: string;
      sourceType: string;
      baseUrl?: string | null;
      defaultConfig: Record<string, unknown>;
      active: boolean;
      editable: boolean;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/connectors/templates').then((r) => r.data),
  createConnectorTemplate: (data: {
    templateKey: string;
    displayName: string;
    description?: string;
    sourceType?: string;
    baseUrl?: string;
    defaultConfig: Record<string, unknown>;
    active?: boolean;
  }) => client.post('/pm/connectors/templates', data).then((r) => r.data),
  updateConnectorTemplate: (id: number, data: Record<string, unknown>) =>
    client.patch(`/pm/connectors/templates/${id}`, data).then((r) => r.data),
  deleteConnectorTemplate: (id: number) =>
    client.delete(`/pm/connectors/templates/${id}`).then((r) => r.data),
  createConnector: (data: {
    connectorKey: string;
    displayName: string;
    connectorType: string;
    mode?: string;
    baseUrl?: string;
    apiPriority?: number;
    config?: Record<string, unknown>;
    enabled?: boolean;
  }) => client.post('/pm/connectors', data).then((r) => r.data),
  updateConnector: (id: number, data: Record<string, unknown>) =>
    client.patch(`/pm/connectors/${id}`, data).then((r) => r.data),
  validateConnector: (id: number, data?: {
    sourceUrl?: string;
    sampleLimit?: number;
    config?: Record<string, unknown>;
  }) =>
    client.post<{ ok: boolean; fetchedCount: number; sampleCount: number; samples: Array<{
      title?: string | null;
      contentPreview: string;
      url?: string | null;
      author?: string | null;
    }>; warnings: string[] }>(`/pm/connectors/${id}/validate`, data ?? {}).then((r) => r.data),
  listConnectorProfiles: (params?: { page?: number; per_page?: number; connector_id?: number; active?: boolean }) =>
    client.get<{ items: Array<{
      id: number;
      connectorId: number;
      profileName: string;
      version: number;
      profile: Record<string, unknown>;
      active: boolean;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/connector-profiles', { params }).then((r) => r.data),
  createConnectorProfile: (data: {
    connectorId: number;
    profileName: string;
    version?: number;
    profile: Record<string, unknown>;
    active?: boolean;
  }) => client.post('/pm/connector-profiles', data).then((r) => r.data),
  updateConnectorProfile: (id: number, data: Record<string, unknown>) =>
    client.patch(`/pm/connector-profiles/${id}`, data).then((r) => r.data),
  deleteConnectorProfile: (id: number) =>
    client.delete(`/pm/connector-profiles/${id}`).then((r) => r.data),
  listExtractionRules: (params?: {
    page?: number;
    per_page?: number;
    source_id?: number;
    connector_id?: number;
    active?: boolean;
  }) =>
    client.get<{ items: Array<{
      id: number;
      sourceId?: number | null;
      connectorId?: number | null;
      ruleName: string;
      format: string;
      priority: number;
      active: boolean;
      rule: Record<string, unknown>;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/extraction-rules', { params }).then((r) => r.data),
  createExtractionRule: (data: {
    sourceId?: number;
    connectorId?: number;
    ruleName: string;
    format?: string;
    priority?: number;
    active?: boolean;
    rule: Record<string, unknown>;
  }) => client.post('/pm/extraction-rules', data).then((r) => r.data),
  updateExtractionRule: (id: number, data: Record<string, unknown>) =>
    client.patch(`/pm/extraction-rules/${id}`, data).then((r) => r.data),
  deleteExtractionRule: (id: number) =>
    client.delete(`/pm/extraction-rules/${id}`).then((r) => r.data),

  listCollectionJobs: (params?: { page?: number; per_page?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      jobName: string;
      countries: unknown;
      sourceIds: unknown;
      cronExpr: string;
      lookbackDays: number;
      maxSources: number;
      maxSignalsPerSource: number;
      autoDiscovery: boolean;
      enabled: boolean;
      lastRunAt?: string | null;
      nextRunAt?: string | null;
      createdBy?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/collection/jobs', { params }).then((r) => r.data),
  createCollectionJob: (data: Record<string, unknown>) =>
    client.post('/pm/collection/jobs', data).then((r) => r.data),
  updateCollectionJob: (id: number, data: Record<string, unknown>) =>
    client.patch(`/pm/collection/jobs/${id}`, data).then((r) => r.data),
  deleteCollectionJob: (id: number) =>
    client.delete(`/pm/collection/jobs/${id}`).then((r) => r.data),
  runCollectionJobNow: (id: number) =>
    client.post<{ runId: number; discoveredClusters: number; discoveredIdeas: number }>(`/pm/collection/jobs/${id}/run-now`).then((r) => r.data),
  dryRunCollectionJob: (id: number, data?: { sampleSize?: number }) =>
    client.post<{
      jobId: number;
      sampleSize: number;
      fetchedCount: number;
      parsedCount: number;
      emptyContentCount: number;
      coverage: Record<string, unknown>;
      samples: Array<{
        title?: string | null;
        contentPreview: string;
        url?: string | null;
        author?: string | null;
      }>;
      warnings: string[];
    }>(`/pm/collection/jobs/${id}/dry-run`, data ?? {}).then((r) => r.data),
  runDueCollectionJobs: () =>
    client.post<{
      attemptedJobs: number;
      startedRuns: number;
      failedJobs: number;
      totalDiscoveredClusters: number;
      totalDiscoveredIdeas: number;
      errors: string[];
    }>('/pm/collection/jobs/run-due').then((r) => r.data),
  runIntentCollection: (data: {
    intent: string;
    countryCode: string;
    lookbackDays?: number;
    maxSources?: number;
    maxSignalsPerSource?: number;
    autoDiscovery?: boolean;
  }) =>
    client.post<{
      intent: string;
      countryCode: string;
      jobId: number;
      runId: number;
      plannedSources: Array<{
        sourceId: number;
        sourceName: string;
        sourceType: string;
        baseUrl: string;
        strategy: string;
      }>;
      discoveredClusters: number;
      discoveredIdeas: number;
      warnings: string[];
    }>('/pm/collection/intent-run', data).then((r) => r.data),
  runDiscoveryPipeline: (data?: { lookbackDays?: number; countries?: string[]; maxDocuments?: number }) =>
    client.post<{
      modelRunId: number;
      processedDocuments: number;
      generatedClusters: number;
      promotedIdeas: number;
    }>('/pm/discovery/pipelines/run', data ?? {}).then((r) => r.data),
  listDemandClusters: (params?: { page?: number; per_page?: number; country_code?: string; status?: string }) =>
    client.get<{ items: Array<{
      id: number;
      countryCode?: string | null;
      themeKey: string;
      title: string;
      summary?: string | null;
      confidence: number;
      novelty: number;
      impact: number;
      actionability: number;
      signalCount: number;
      firstSeenAt?: string | null;
      lastSeenAt?: string | null;
      status: string;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/demand-clusters', { params }).then((r) => r.data),
  promoteDemandClusterIdea: (id: number, data?: { ideaTitle?: string; ideaDesc?: string; ownerId?: string }) =>
    client.post<{ clusterId: number; ideaId: number }>(`/pm/demand-clusters/${id}/promote-idea`, data ?? {}).then((r) => r.data),
  reviewIdea: (id: number, data: { action: 'approve' | 'reject' | 'merge' | 'duplicate'; reason?: string; mergeTargetIdeaId?: number; payload?: Record<string, unknown> }) =>
    client.post<{ reviewId: number; ideaId: number; action: string }>(`/pm/ideas/${id}/review`, data).then((r) => r.data),
  labelFeedback: (data: { targetType: 'signal' | 'cluster' | 'idea'; targetId: string; labelKey: string; labelValue: string; confidence?: number; comment?: string }) =>
    client.post<{ id: number; accepted: boolean }>('/pm/feedback/labels', data).then((r) => r.data),

  listInsights: (params?: { page?: number; per_page?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      countryCode: string;
      title: string;
      summary?: string | null;
      signalCount: number;
      trendScore: number;
      businessImpactScore: number;
      confidence: number;
      evidenceCount: number;
      status: string;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/insights', { params }).then((r) => r.data),
  createInsight: (data: {
    countryCode: string;
    title: string;
    summary?: string;
    signalCount?: number;
    trendScore?: number;
    businessImpactScore?: number;
    confidence?: number;
    evidenceCount?: number;
    status?: string;
  }) => client.post('/pm/insights', data).then((r) => r.data),
  updateInsight: (id: number, data: Record<string, unknown>) =>
    client.patch(`/pm/insights/${id}`, data).then((r) => r.data),
  deleteInsight: (id: number) => client.delete(`/pm/insights/${id}`).then((r) => r.data),

  listIdeas: (params?: { page?: number; per_page?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      countryCode: string;
      clusterId?: number | null;
      ideaTitle: string;
      ideaDesc?: string | null;
      priorityScore: number;
      confidence: number;
      evidenceCount: number;
      status: string;
      ownerId?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/ideas', { params }).then((r) => r.data),
  createIdea: (data: Record<string, unknown>) => client.post('/pm/ideas', data).then((r) => r.data),
  updateIdea: (id: number, data: Record<string, unknown>) => client.patch(`/pm/ideas/${id}`, data).then((r) => r.data),
  deleteIdea: (id: number) => client.delete(`/pm/ideas/${id}`).then((r) => r.data),

  listBriefJobs: (params?: { page?: number; per_page?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      jobName: string;
      countries: unknown;
      cronExpr: string;
      windowDays: number;
      enabled: boolean;
      lastRunAt?: string | null;
      nextRunAt?: string | null;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/brief-jobs', { params }).then((r) => r.data),
  createBriefJob: (data: Record<string, unknown>) => client.post('/pm/brief-jobs', data).then((r) => r.data),
  updateBriefJob: (id: number, data: Record<string, unknown>) => client.patch(`/pm/brief-jobs/${id}`, data).then((r) => r.data),
  deleteBriefJob: (id: number) => client.delete(`/pm/brief-jobs/${id}`).then((r) => r.data),
  runBriefJobNow: (id: number) =>
    client.post<{
      job: {
        id: number;
        jobName: string;
        countries: unknown;
        cronExpr: string;
        windowDays: number;
        enabled: boolean;
        lastRunAt?: string | null;
        nextRunAt?: string | null;
        createdAt: string;
        updatedAt: string;
      };
      reports: Array<{
        id: number;
        jobId: number;
        countryCode: string;
        reportDate: string;
        reportMarkdown: string;
        topIdeas: unknown;
        evidenceRefs: unknown;
        qualityScore: number;
        createdAt: string;
      }>;
    }>(`/pm/brief-jobs/${id}/run-now`).then((r) => r.data),
  listBriefReports: (params?: { page?: number; per_page?: number; job_id?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      jobId: number;
      countryCode: string;
      reportDate: string;
      reportMarkdown: string;
      topIdeas: unknown;
      evidenceRefs: unknown;
      qualityScore: number;
      createdAt: string;
    }>; total: number }>('/pm/brief-reports', { params }).then((r) => r.data),

  listPrds: (params?: { page?: number; per_page?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      countryCode: string;
      ideaId?: number | null;
      title: string;
      status: string;
      ownerId?: string | null;
      currentVersion: number;
      createdAt: string;
      updatedAt: string;
    }>; total: number }>('/pm/prds', { params }).then((r) => r.data),
  createPrd: (data: Record<string, unknown>) => client.post('/pm/prds', data).then((r) => r.data),
  updatePrd: (id: number, data: Record<string, unknown>) => client.patch(`/pm/prds/${id}`, data).then((r) => r.data),
  deletePrd: (id: number) => client.delete(`/pm/prds/${id}`).then((r) => r.data),

  listFeedback: (params?: { page?: number; per_page?: number; country_code?: string }) =>
    client.get<{ items: Array<{
      id: number;
      targetType: string;
      targetId: string;
      feedbackType: string;
      comment?: string | null;
      createdBy?: string | null;
      createdAt: string;
    }>; total: number }>('/pm/feedback', { params }).then((r) => r.data),
  createFeedback: (data: Record<string, unknown>) => client.post('/pm/feedback', data).then((r) => r.data),
  updateFeedback: (id: number, data: Record<string, unknown>) => client.patch(`/pm/feedback/${id}`, data).then((r) => r.data),
  deleteFeedback: (id: number) => client.delete(`/pm/feedback/${id}`).then((r) => r.data),
};
