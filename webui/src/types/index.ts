// Dashboard types

export interface UsageAlertInfo {
  id: string;
  tenant_id: string;
  name: string;
  alert_type: 'daily_budget' | 'monthly_budget' | 'per_key_limit';
  threshold_tokens: number;
  threshold_usd?: number;
  enabled: boolean;
  notified_at?: string;
  created_at: string;
  created_by?: string;
}

export interface UsageAlertListResponse {
  alerts: UsageAlertInfo[];
  total: number;
}
export interface TokenStats {
  total_input_tokens: number;
  total_output_tokens: number;
  total_cache_creation_tokens: number;
  total_cache_read_tokens: number;
  estimated_cost_usd: number;
  session_count: number;
  total_requests: number;
  active_model_count: number;
}

export interface CacheStats {
  total_cache_creation_tokens: number;
  total_cache_read_tokens: number;
  estimated_savings_usd: number;
  cache_hit_rate: number;
}

export interface DailyTokenStats {
  date: string;
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  estimated_cost_usd: number;
}

export interface ModelUsageStats {
  model: string;
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  estimated_cost_usd: number;
}

export interface ModuleTokenUsageStats {
  module: 'chat' | 'adversarial' | 'analytics' | 'engineering' | 'operations' | 'agent' | string;
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  estimated_cost_usd: number;
  token_share_pct: number;
}

export interface DashboardOverview {
  token_stats: TokenStats;
  cache_stats: CacheStats;
  top_models: ModelUsageStats[];
  daily_trend: DailyTokenStats[];
}

export interface DashboardConfigOverviewStats {
  enabled_api_key_count: number;
  enabled_hook_count: number;
  enabled_mcp_server_count: number;
  active_user_count: number;
}

// Hook types — tool and business lifecycle hooks stored per tenant
export type HookEventType =
  | 'pre_tool_use'
  | 'post_tool_use'
  | 'post_tool_use_failure'
  | 'message_received'
  | 'before_model_call'
  | 'after_model_call'
  | 'before_route'
  | 'after_route'
  | 'before_final_answer'
  | 'after_final_answer'
  | 'task_completed'
  | 'bot_message_received';
export type HookLanguage = 'shell' | 'python';

export interface HookInfo {
  id: string;
  tenant_id: string;
  event_type: HookEventType;
  name: string;
  description?: string;
  scenarios?: string[] | null;
  language: HookLanguage;
  code?: string;
  command: string;
  enabled: boolean;
  priority: number;
  timeout_seconds: number;
  fail_fast: boolean;
  created_at: string;
  updated_at: string;
}

export interface HookValidationError {
  line?: number;
  column?: number;
  message: string;
}

export interface HookValidationResponse {
  valid: boolean;
  errors: HookValidationError[];
  warnings: string[];
}

export interface HookLogEntry {
  id: string;
  hook_id: string;
  event_type: HookEventType;
  scenario?: string | null;
  tool_name: string;
  input_json?: string | null;
  output_json?: string | null;
  exit_code?: number | null;
  duration_ms?: number | null;
  error_message?: string | null;
  executed_at: string;
}

// Bot Gateway
export interface BotAgentCapabilityInfo {
  id: string;
  agent_id: string;
  capability_key: string;
  enabled: boolean;
  config_json?: Record<string, unknown> | null;
}

export interface BotAgentInfo {
  id: string;
  tenant_id: string;
  name: string;
  description?: string | null;
  enabled: boolean;
  default_capability: string;
  persona_prompt?: string | null;
  capabilities: BotAgentCapabilityInfo[];
  channels_count: number;
  created_by?: string | null;
  created_at: string;
  updated_at: string;
}

export interface BotAgentChannelInfo {
  id: string;
  agent_id: string;
  platform: string;
  name: string;
  enabled: boolean;
  inbound_mode: string;
  inbound_secret_set: boolean;
  inbound_status: string;
  inbound_error?: string | null;
  inbound_last_seen_at?: string | null;
  inbound_last_message_at?: string | null;
  outbound_webhook_url?: string | null;
  outbound_token_set: boolean;
  signing_secret_set: boolean;
  outbound_signing_secret_set: boolean;
  config_json?: Record<string, unknown> | null;
  created_at: string;
  updated_at: string;
}

export interface BotMessageLogInfo {
  id: string;
  agent_id?: string | null;
  channel_id?: string | null;
  agent_task_id?: string | null;
  direction: string;
  platform: string;
  external_user_id?: string | null;
  external_conversation_id?: string | null;
  message_type: string;
  content_json?: Record<string, unknown> | null;
  status: string;
  queue_status: string;
  attempt_count: number;
  max_attempts: number;
  claimed_by?: string | null;
  claimed_at?: string | null;
  last_error?: string | null;
  finished_at?: string | null;
  error_message?: string | null;
  created_at: string;
}

export interface BotExternalIdentityInfo {
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

// MCP types

/** Authentication configuration returned from the backend (token value is always redacted). */
export interface McpToolInfo {
  name: string;
  description?: string;
  inputSchema?: Record<string, unknown>;
}

export interface McpResourceInfo {
  uri: string;
  name?: string;
  description?: string;
  mimeType?: string;
}

export interface McpPromptInfo {
  name: string;
  description?: string;
  arguments?: Array<{
    name: string;
    description?: string;
    required?: boolean;
  }>;
}

export interface McpServerAuthInfo {
  auth_type: string;
  has_token: boolean;
  extra_headers?: Record<string, string>;
  timeout_ms?: number;
}

export type McpConnectionStatus = 'disconnected' | 'connected' | 'error';

export interface McpServerInfo {
  name: string;
  transport: string;
  command?: string;
  args: string[];
  /** Environment values are redacted when returned by the API. */
  env?: Record<string, string>;
  url?: string;
  enabled: boolean;
  tools_count: number;
  /** WebUI-tested connectivity result. */
  connection_status: McpConnectionStatus;
  /** Runtime health during session discovery. */
  status: string;
  last_error?: string;
  auth: McpServerAuthInfo;
  /** Tools exposed by this MCP server. */
  tools?: McpToolInfo[];
  /** Resources exposed by this MCP server. */
  resources?: McpResourceInfo[];
  /** Prompts exposed by this MCP server. */
  prompts?: McpPromptInfo[];
}

export interface McpStats {
  total_servers: number;
  healthy_servers: number;
  total_tools: number;
  transport_distribution: Record<string, number>;
}

export interface EmbeddingHealthAnn {
  state: string;
  reason?: string | null;
  loaded_in_memory: boolean;
  base_points: number;
  overlay_points: number;
  stale_points: number;
  disk_artifacts_present: boolean;
  snapshot_pending: boolean;
}

export interface EmbeddingHealthResponse {
  status: string;
  total_vectors?: number;
  reason?: string;
  ann?: EmbeddingHealthAnn;
}

export interface EmbeddingConfigResponse {
  available: boolean;
  model: string | null;
  base_url: string | null;
  configured_via: string;
  dimensions?: number | null;
  api_configured: boolean;
  local_model: string;
  profiles: Array<{
    datasourceId: string;
    kind: 'api' | 'local';
    activeProfileId: string | null;
    desiredProfileId: string | null;
    status: 'pending' | 'building' | 'ready' | 'degraded' | 'failed' | 'disabled';
    indexedItems: number;
    totalItems: number;
    lastError: string | null;
    model: string | null;
    provider: string | null;
    healthStatus: string | null;
    circuitOpenUntil: string | null;
  }>;
}

// Skill types
export interface SkillInfo {
  id: string;
  name: string;
  description?: string;
  path: string;
  source: 'uploaded' | 'marketplace' | 'builtin';
  marketplaceOrigin?: {
    repoFullName: string;
    repoUrl: string;
    branch: string;
    skillName: string;
    skillPath: string;
    readmeUrl?: string | null;
    htmlUrl?: string | null;
    sourceType: string;
  } | null;
  tags: string[];
  enabled: boolean;
  version: string;
  file_size?: number;
  commands_count: number;
  created_by?: string;
  created_at: string;
  updated_at: string;
}

export interface UploadZipResult {
  skill: SkillInfo;
  warnings: string[];
  securityScan: SkillSecurityScan;
  name: string;
  description?: string;
  tags: string[];
}

export type SkillSecurityScanStatus = 'passed' | 'warning' | 'blocked' | 'ai_unavailable';
export type SkillSecuritySeverity = 'low' | 'medium' | 'high' | 'critical';

export interface SkillSecurityFinding {
  source: 'ai' | 'rule';
  severity: SkillSecuritySeverity;
  category:
    | 'credential'
    | 'command_execution'
    | 'data_exfiltration'
    | 'prompt_injection'
    | 'filesystem'
    | 'network'
    | 'dependency'
    | 'other';
  file: string;
  evidence: string;
  recommendation: string;
}

export interface SkillSecurityScan {
  status: SkillSecurityScanStatus;
  summary: string;
  findings: SkillSecurityFinding[];
  aiScanned: boolean;
  requiresConfirmation: boolean;
}

export interface SkillCommand {
  name: string;
  path: string;
  size?: number;
}

export interface SkillDetail {
  info: SkillInfo;
  readme: string;
  commands: SkillCommand[];
}

// API Key types
export interface ApiKeyRecord {
  id: string;
  name: string;
  provider: string;
  base_url?: string;
  model?: string;
  dimensions?: number | null;
  audio_generate_path?: string;
  audio_query_path?: string;
  /** Key type: 'chat' | 'embedding' | 'image' | 'video' | 'audio'. */
  model_type: string;
  key_hint: string;
  daily_limit?: number;
  monthly_limit?: number;
  enabled: boolean;
  priority?: number;
  is_primary?: boolean;
  input_price_per_million?: number;
  output_price_per_million?: number;
  /** JSON array of scenario tags, e.g. ["chat","nl2sql","rd","pm"]. Null means legacy/all scenarios. */
  scenarios?: string[] | null;
  capabilities_json?: {
    reasoningEffort?: boolean;
    reasoningEffortDefault?: string;
    reasoningTransport?: string;
    reasoningEffortValues?: string[];
    reasoningBudgetMap?: Record<string, string>;
    reasoningPolicy?: 'auto' | 'fast' | 'standard' | 'deep' | 'maximum';
    includeReasoning?: boolean;
    useMaxCompletionTokens?: boolean;
    outputTokenParameter?: string;
    supportsTemperature?: boolean;
    supportsTopP?: boolean;
    protocol?: string;
    capabilitySchemaVersion?: number;
    registryVersion?: string;
    features?: {
      streaming?: boolean;
      tools?: boolean;
      parallelTools?: boolean;
      structuredOutput?: boolean;
      jsonObject?: boolean;
      jsonSchema?: boolean;
      strictJsonSchema?: boolean;
      structuredOutputVerified?: boolean;
      structuredOutputVerifiedMode?: string;
      structuredOutputVerifiedAt?: string;
      vision?: boolean;
      nativeWebSearch?: boolean;
      reasoningOutput?: boolean;
    };
    nativeWebSearch?: {
      enabled?: boolean;
      mode?: string;
      extraBody?: Record<string, unknown>;
      toolTemplate?: Record<string, unknown>;
    };
    contextWindowTokens?: number;
    maxOutputTokens?: number;
  } | null;
  resolved_capabilities?: ApiKeyRecord['capabilities_json'];
  model_profile?: {
    id: string;
    protocol: string;
    source: string;
    confidence: string;
    detectionStatus: string;
    detectedAt?: string | null;
    expiresAt?: string | null;
  } | null;
  /** True only when the backend can decrypt and use this key at runtime. */
  runtime_available?: boolean;
  /** Backend diagnostic when runtime_available is false. */
  runtime_error?: string | null;
  created_at: string;
  usage_today?: number;
  usage_month?: number;
  /** ISO 8601 expiry date (null = never expires). */
  expires_at?: string | null;
  /** Last time this key was used. */
  last_used_at?: string | null;
  /** Usage in the last 7 days. */
  usage_7d?: number;
}

export interface ApiKeyStats {
  key_id: string;
  total_calls: number;
  total_tokens: number;
  total_cost_usd: number;
  daily_usage: UsageRecord[];
}

// AOS Code Studio / R&D module
export interface RdRepository {
  id: string;
  name: string;
  url: string;
  branch: string;
  description?: string | null;
  isCloned: boolean;
  clonePath?: string | null;
  lastSyncAt?: string | null;
  createdAt: string;
  defaultTestCommand?: string | null;
  defaultBuildCommand?: string | null;
  indexStatus?: string | null;
  indexedFileCount: number;
  indexedSymbolCount: number;
  indexedImportCount: number;
  detectedLanguages: Array<{ language: string; fileCount: number }>;
  detectedStack: string[];
  detectedTestCommand?: string | null;
  detectedBuildCommand?: string | null;
  autoSyncEnabled: boolean;
  autoSyncIntervalMinutes: number;
  lastAutoSyncAt?: string | null;
  lastSyncError?: string | null;
}

export interface RdRepositoryListResponse {
  repositories: RdRepository[];
  total: number;
}

export interface RdFileNode {
  name: string;
  path: string;
  nodeType: 'dir' | 'file' | string;
  sizeBytes?: number | null;
  language?: string | null;
  changeCount?: number;
  pendingCount?: number;
  children?: RdFileNode[] | null;
}

export interface RdWorkbenchChangedFileGroup {
  changeType: string;
  count: number;
  pendingCount: number;
  files: Array<{
    id: string;
    filePath: string;
    applied: boolean;
  }>;
}

export interface RdFileContentResponse {
  path: string;
  content: string;
  sizeBytes: number;
  language?: string | null;
}

export interface RdRepositorySearchHit {
  path: string;
  lineNumber: number;
  snippet: string;
}

export interface RdRepositoryFileSuggestion {
  path: string;
  name: string;
  language?: string | null;
  sizeBytes?: number | null;
}

export type RdBaselinePolicy = 'current_worktree' | 'head';

export interface RdRepositoryWorktreeStatus {
  repositoryId: string;
  headSha?: string | null;
  dirty: boolean;
  dirtyPathCount: number;
  trackedModifiedCount: number;
  untrackedCount: number;
  dirtyPathsSample: string[];
  statusShort: string;
  defaultBaselinePolicy: RdBaselinePolicy | string;
}

export interface RdRepositorySymbol {
  id: number;
  filePath: string;
  language?: string | null;
  symbolName: string;
  symbolKind: string;
  signature?: string | null;
  lineNumber: number;
}

export interface RdRepositoryImport {
  id: number;
  filePath: string;
  language?: string | null;
  importPath: string;
  importKind: string;
  lineNumber: number;
}

export type RdCodeIntelAction =
  | 'definition'
  | 'references'
  | 'hover'
  | 'document_symbols'
  | 'workspace_symbols'
  | 'diagnostics';

export interface RdCodeIntelLanguageStatus {
  language: string;
  status: string;
  serverCommand?: string | null;
  installed: boolean;
  lastError?: string | null;
  updatedAt?: string | null;
}

export interface RdCodeIntelStatusResponse {
  repositoryId: string;
  rootPath: string;
  languages: RdCodeIntelLanguageStatus[];
  fallbackAvailable: boolean;
}

export interface RdCodeIntelLocation {
  path: string;
  line: number;
  character: number;
  endLine?: number | null;
  endCharacter?: number | null;
  preview?: string | null;
}

export interface RdCodeIntelQueryResponse {
  source: 'lsp' | 'symbol_index' | 'rg' | 'none' | string;
  status: 'ok' | 'degraded' | 'not_found' | 'error' | string;
  language?: string | null;
  locations: RdCodeIntelLocation[];
  hover?: {
    content: string;
    language?: string | null;
  } | null;
  diagnostics: Array<Record<string, unknown>>;
  message?: string | null;
}

export interface RdPreviewSession {
  id: string;
  repositoryId: string;
  taskId?: string | null;
  runtimeSessionId?: string | null;
  processId?: string | null;
  command: string;
  port?: number | null;
  path: string;
  url?: string | null;
  proxiedUrl?: string | null;
  status: string;
  lastError?: string | null;
  logsPreview?: string | null;
  startedAt?: string | null;
  stoppedAt?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface RdPreviewEvent {
  id: string;
  eventType: string;
  severity: string;
  message: string;
  metadataJson?: Record<string, unknown> | null;
  createdAt: string;
}

export interface RdPreviewLogsResponse {
  session: RdPreviewSession;
  events: RdPreviewEvent[];
}

export type RdTaskMode = 'ask' | 'modify' | 'explain' | 'review';

export interface RdIntentRouteResponse {
  mode: RdTaskMode | string;
  confidence: number;
  reason?: string | null;
  source: 'llm' | 'fallback' | string;
  model?: string | null;
  profile?: string | null;
  profileName?: string | null;
  depth?: string | null;
  shouldDeepScan?: boolean;
}

export interface RdTask {
  id: string;
  threadId?: string | null;
  parentTaskId?: string | null;
  iterationNo?: number;
  threadTitle?: string | null;
  repositoryId?: string | null;
  specId?: string | null;
  agentProfileId?: string | null;
  workflowId?: string | null;
  runtimeSessionId?: string | null;
  mode: RdTaskMode | string;
  contextProfile?: string | null;
  contextProfileName?: string | null;
  contextDepth?: string | null;
  shouldDeepScan?: boolean;
  status: string;
  title: string;
  prompt: string;
  model?: string | null;
  planMd?: string | null;
  answerMd?: string | null;
  reviewMd?: string | null;
  prTitle?: string | null;
  prDescription?: string | null;
  errorMessage?: string | null;
  createdAt: string;
  updatedAt: string;
  completedAt?: string | null;
}

export interface RdTaskListResponse {
  tasks: RdTask[];
  total: number;
}

export interface RdTaskEvent {
  id: number;
  stage: string;
  status: string;
  message?: string | null;
  detailJson?: Record<string, unknown> | null;
  createdAt: string;
}

export interface RdTaskEventListResponse {
  events: RdTaskEvent[];
  hasMore: boolean;
  nextCursor?: number | null;
  pageSize: number;
}

export interface RdFileChange {
  id: string;
  taskId: string;
  repositoryId?: string | null;
  filePath: string;
  changeType: string;
  diffPatch: string;
  applied: boolean;
  appliedAt?: string | null;
  createdAt: string;
}

export interface RdApplyHunksResponse {
  appliedHunks: number;
  totalHunks: number;
  remainingChangeId?: string | null;
}

export interface RdTaskRollbackResponse {
  rolledBack: number;
  skipped: number;
}

export interface RdTestRun {
  id: string;
  taskId: string;
  repositoryId?: string | null;
  command: string;
  status: string;
  exitCode?: number | null;
  stdoutText?: string | null;
  stderrText?: string | null;
  durationMs?: number | null;
  createdAt: string;
}

export interface RdWorkbenchRuntimeSession {
  id: string;
  status: string;
  workspaceRoot: string;
  isolationMode: string;
  cancelRequested: boolean;
  heartbeatAt?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
}

export interface RdWorkbenchRuntimeProcess {
  id: string;
  command: string;
  cwd?: string | null;
  status: string;
  pid?: number | null;
  processGroupId?: number | null;
  exitCode?: number | null;
  stdoutPreview?: string | null;
  stderrPreview?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
}

export interface RdWorkbenchRuntimeArtifact {
  id: string;
  artifactType: string;
  path?: string | null;
  contentText?: string | null;
  contentHash?: string | null;
  sizeBytes: number;
  createdAt?: string | null;
}

export interface RdWorkbenchAgentTask {
  id: string;
  status: string;
  phase?: string | null;
  progressPercent?: number | null;
  title?: string | null;
  lastEvent?: string | null;
  errorMessage?: string | null;
  createdAt?: string | null;
  updatedAt?: string | null;
}

export interface RdWorkbenchTraceEvent {
  id: string;
  taskId?: string;
  eventType: string;
  phase?: string | null;
  status?: string | null;
  severity?: string | null;
  message: string;
  metadataJson?: Record<string, unknown> | null;
  runtimeSessionId?: string | null;
  runtimeProcessId?: string | null;
  tokenInput?: number | null;
  tokenOutput?: number | null;
  durationMs?: number | null;
  createdAt: string;
}

export interface RdTaskWorkbenchResponse {
  task: RdTask;
  agentTask?: RdWorkbenchAgentTask | null;
  runtimeSession?: RdWorkbenchRuntimeSession | null;
  runtimeProcesses: RdWorkbenchRuntimeProcess[];
  runtimeArtifacts: RdWorkbenchRuntimeArtifact[];
  traceEvents: RdWorkbenchTraceEvent[];
  rdEvents: Array<Record<string, unknown>>;
  fileChanges: RdFileChange[];
  testRuns: RdTestRun[];
  fileTree?: RdFileNode[];
  changedFileGroups?: RdWorkbenchChangedFileGroup[];
  activeRuntimeCommand?: RdWorkbenchRuntimeProcess | null;
  terminalOutputPreview?: {
    processId?: string | null;
    command?: string | null;
    status?: string | null;
    stdoutPreview?: string | null;
    stderrPreview?: string | null;
    exitCode?: number | null;
  } | null;
  linkedSpec?: {
    specId: string;
    title?: string | null;
    stage?: string | null;
    status?: string | null;
    taskItemId?: string | null;
    linkStatus?: string | null;
    updatedAt?: string | null;
  } | null;
  latestAnswer?: string | null;
  suggestedActions: string[];
}

export interface RdFailedCommand {
  command: string;
  failureCount: number;
}

export interface RdQualitySummary {
  days: number;
  repositoryId?: string | null;
  taskCount: number;
  completedCount: number;
  failedCount: number;
  runningCount: number;
  waitingApprovalCount: number;
  cancelledCount: number;
  successRate: number;
  avgTaskDurationMs?: number | null;
  diffCount: number;
  pendingDiffCount: number;
  appliedDiffCount: number;
  testRunCount: number;
  passedTestCount: number;
  failedTestCount: number;
  testPassRate: number;
  candidateWorktreeStartedCount: number;
  candidateWorktreeDiffCount: number;
  candidateWorktreeNoDiffCount: number;
  candidateWorktreeFailedCount: number;
  candidateContextSyncedCount: number;
  candidateContextSyncFailedCount: number;
  candidateDiffBytes: number;
  diffCheckPassedCount: number;
  diffCheckFailedCount: number;
  diffCheckSkippedCount: number;
  diffCheckPassRate: number;
  diffRepairPassedCount: number;
  diffRepairFailedCount: number;
  reviewAgentRunCount: number;
  reviewFindingsCount: number;
  reviewFileRefCount: number;
  reviewLineRefCount: number;
  embeddingStoreEnabled: boolean;
  embeddingModel?: string | null;
  embeddingChunkCount: number;
  embeddingContextSummaryChunkCount: number;
  embeddingFileSummaryChunkCount: number;
  embeddingSymbolChunkCount: number;
  embeddingImportChunkCount: number;
  embeddingTaskChunkCount: number;
  repositorySummaryCount: number;
  directorySummaryCount: number;
  fileSummaryCount: number;
  llmSummaryCount: number;
  staleSummaryCount: number;
  retrievalEvidenceEventCount: number;
  cacheUsageEventCount: number;
  retrievalSelectedFileCount: number;
  cacheHitRate: number;
  embeddingHitCount: number;
  summaryHitCount: number;
  symbolHitCount: number;
  importHitCount: number;
  dependencyGraphHitCount: number;
  taskMemoryHitCount: number;
  readFileCount: number;
  grepSearchCount: number;
  globSearchCount: number;
  repeatedToolTargetCount: number;
  cacheReusedChunkCount: number;
  cacheRegeneratedChunkCount: number;
  embeddingReusedChunkCount: number;
  embeddingRegeneratedChunkCount: number;
  embeddingPrunedChunkCount: number;
  fileSummaryReusedCount: number;
  fileSummaryRegeneratedCount: number;
  estimatedTokensSaved: number;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  totalTokens: number;
  embeddingTokens: number;
  contextPlannerTokens: number;
  runtimeTokens: number;
  topFailedCommands: RdFailedCommand[];
  generatedAt: string;
}

export type RdStudioMode = 'code' | 'spec';

export type RdSpecStage =
  | 'intake'
  | 'spec'
  | 'design'
  | 'tasks'
  | 'implementation'
  | 'verify'
  | 'final';

export interface RdSpecTaskItem {
  id: string;
  title: string;
  description: string;
  status: 'pending' | 'running' | 'waiting_approval' | 'completed' | 'failed' | 'skipped' | string;
  priority: 'p0' | 'p1' | 'p2' | string;
  linkedRdTaskId?: string | null;
  acceptance?: string[];
}

export interface RdSpec {
  id: string;
  repositoryId?: string | null;
  repositoryIds?: string[];
  title: string;
  prompt: string;
  requirementsMd?: string | null;
  designMd?: string | null;
  tasksMd?: string | null;
  acceptanceMd?: string | null;
  status: string;
  mode?: RdStudioMode | string;
  currentStage?: RdSpecStage | string;
  specVersion?: number;
  designVersion?: number;
  tasksVersion?: number;
  approvedRequirementsAt?: string | null;
  approvedDesignAt?: string | null;
  approvedTasksAt?: string | null;
  approvedBy?: string | null;
  stageStatusJson?: Record<string, unknown> | null;
  taskItems?: RdSpecTaskItem[];
  implementationSummaryJson?: Record<string, unknown> | null;
  linkedAgentTaskId?: string | null;
  lastError?: string | null;
  model?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface RdSpecEvent {
  id: string;
  specId: string;
  eventType: string;
  stage?: string | null;
  status?: string | null;
  message: string;
  metadataJson?: Record<string, unknown> | null;
  createdAt: string;
}

export interface RdAgentProfile {
  id: string;
  name: string;
  rolePrompt: string;
  allowedTools?: string | Record<string, unknown> | unknown[] | null;
  defaultModel?: string | null;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface RdAgentWorkflow {
  id: string;
  name: string;
  description?: string | null;
  definitionJson: Record<string, unknown>;
  source: string;
  sourceItemId?: string | null;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface RdAgentMarketItem {
  id: string;
  itemType: 'agent' | 'workflow' | string;
  name: string;
  description: string;
  tags: string[];
  source: string;
  installed: boolean;
  installTargetId?: string | null;
}

export interface RdAgentMarketSearchResponse {
  total: number;
  items: RdAgentMarketItem[];
}

export interface RdAgentMarketInstallResponse {
  item: RdAgentMarketItem;
  agentProfile?: RdAgentProfile | null;
  workflow?: RdAgentWorkflow | null;
}

export interface RdSteeringRule {
  id: string;
  repositoryId?: string | null;
  repositoryIds?: string[];
  name: string;
  description?: string | null;
  contentMd: string;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface RdIntegration {
  id: string;
  provider: 'github' | 'gitlab' | 'jira' | 'sentry' | 'custom' | string;
  name: string;
  configJson?: Record<string, unknown> | null;
  enabled: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface RdIntegrationTestResult {
  ok: boolean;
  provider: string;
  message: string;
  checkedUrl?: string | null;
  statusCode?: number | null;
  detailJson?: Record<string, unknown> | null;
}

export interface RdPrDraftChange {
  filePath: string;
  changeType: string;
  applied: boolean;
}

export interface RdPrDraftTest {
  command: string;
  status: string;
  exitCode?: number | null;
  durationMs?: number | null;
}

export interface RdPrDraft {
  taskId: string;
  title: string;
  description: string;
  branchName: string;
  baseBranch: string;
  repositoryId?: string | null;
  repositoryName?: string | null;
  repositoryUrl?: string | null;
  changes: RdPrDraftChange[];
  tests: RdPrDraftTest[];
  providerPayloads: Record<string, unknown>[];
  markdown: string;
}

export interface RdPrDraftPublishResult {
  ok: boolean;
  provider: string;
  integrationId: string;
  remoteUrl?: string | null;
  statusCode?: number | null;
  message: string;
  responseJson?: Record<string, unknown> | null;
  draft: RdPrDraft;
}

export interface UsageRecord {
  date: string;
  calls: number;
  tokens_used: number;
  cost_usd: number;
}

// Session types
export interface SessionSummary {
  session_id: string;
  path: string;
  message_count: number;
  created_at?: string;
  updated_at?: string;
  model?: string;
  compact_threshold?: number;
}

// Auth types
export interface UserInfo {
  id: string;
  email: string;
  name: string;
  role: string;
  tenant_id?: string;
  is_active?: boolean;
  permission_mode?: string;
  menu_permissions?: string[];
  menu_permissions_inherited?: boolean;
  created_at?: string;
  created_by?: string;
  last_login_at?: string | null;
  password_changed_at?: string | null;
}

export interface LoginResponse {
  token: string;
  user: UserInfo;
  tenant_id?: string;
}

export interface SetupStatusResponse {
  initialized: boolean;
  tenant_count: number;
  user_count: number;
}

export interface SetupResponse {
  tenant_id: string;
  admin_user_id: string;
  token: string;
}

export interface UserListResponse {
  users: UserInfo[];
  total: number;
}

export interface InviteUserResponse {
  user_id: string;
  invite_token: string;
  invite_url: string;
  email_configured: boolean;
  email_sent: boolean;
  email_error?: string | null;
}

export interface NotificationInfo {
  id: string;
  title: string;
  body: string;
  level: 'info' | 'warning' | 'error' | 'success';
  read: boolean;
  created_at: string;
}

export interface NotificationListResponse {
  notifications: NotificationInfo[];
  total: number;
  unread_count: number;
}

// Config types
export interface ConfigSnapshot {
  path: string;
  source: string;
  content: Record<string, unknown>;
}

export interface ConfigOverview {
  configs: ConfigSnapshot[];
  permission_mode?: string;
  current_model?: string;
  active_plugins: string[];
  active_mcp_servers: string[];
}

export interface FeatureFlags {
  hooks_enabled: boolean;
  plugins_enabled: boolean;
  mcp_enabled: boolean;
  telemetry_enabled: boolean;
}

// ---- Chat types ----
export type ContentBlockType = 'text' | 'image' | 'document';

export interface TextBlock {
  type: 'text';
  text: string;
}

export interface ImageBlock {
  type: 'image';
  fileId?: string;
  media_type: string;
  sourceType: 'base64' | 'url';
  data: string;
  name?: string;
  sizeBytes?: number;
  /** Local browser-only preview url (blob/data url). */
  previewUrl?: string;
}

export interface DocumentBlock {
  type: 'document';
  fileId?: string;
  media_type: string;
  sourceType: 'base64' | 'url';
  data: string;
  name?: string;
  sizeBytes?: number;
}

export type ContentBlock = TextBlock | ImageBlock | DocumentBlock;

export interface ChatMessage {
  role: 'user' | 'assistant' | 'system';
  /** Plain string (legacy) or ContentBlock[] (new format). */
  content: string | ContentBlock[];
  /** Tool calls associated with this assistant message. */
  toolCalls?: ToolCallDisplay[];
  /** Whether this message is bookmarked by the user. */
  isBookmarked?: boolean;
  /** Branch parent: if set, this message is a child of another session's message. */
  branchFrom?: string;
}

export interface ToolCallDisplay {
  index: number;
  name: string;
  source: 'mcp' | 'builtin' | 'skill';
  /** For MCP tools: the server name (e.g. 'github'). */
  mcpServer?: string;
  /** For skill tools: the skill name (e.g. 'alibaba-find-skills'). */
  skillName?: string;
  args: string;
  result: string;
  isError: boolean;
  status: 'pending' | 'running' | 'success' | 'error';
  durationMs?: number;
}

export interface ChatUsage {
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  estimatedCostUsd: number;
  model: string;
}

export interface SendMessageResponse {
  sessionId: string;
  message: ChatMessage;
  usage?: ChatUsage;
}

export interface ChatSessionInfo {
  sessionId: string;
  messageCount: number;
  lastUpdated: number;
}

export type StreamEventType =
  | 'content_block_start'
  | 'content_block_delta'
  | 'message_delta'
  | 'stream_end'
  | 'error'
  | 'ping';

export interface ContentBlockStart {
  type: 'content_block_start';
  index: number;
  blockType: 'text' | 'tool_use' | 'thinking' | 'redacted_thinking';
}

export interface ContentBlockDelta {
  type: 'content_block_delta';
  index: number;
  delta: { type: 'text_delta'; text: string };
}

export interface MessageDelta {
  type: 'message_delta';
  usage: {
    inputTokens: number;
    outputTokens: number;
    cacheCreationInputTokens: number;
    cacheReadInputTokens: number;
  };
  stopReason?: string;
}

export interface StreamEnd {
  type: 'stream_end';
  session_id: string;
}

export interface StreamError {
  type: 'error';
  error: string;
}

export type StreamEvent =
  | ContentBlockStart
  | ContentBlockDelta
  | MessageDelta
  | StreamEnd
  | StreamError;

// ---- Upload types ----
export interface UploadResponse {
  fileId: string;
  filename: string;
  mediaType: string;
  size: number;
  url: string;
}

// =============================================================================
// Agent Gateway types — full agent runtime support
// =============================================================================

export interface AgentSession {
  session_id: string;
  name: string;
  user_id: string;
  tenant_id: string;
  workspace: string;
  model: string;
  created_at: string;
  is_pinned: boolean;
  /** MCP servers active in this session. */
  mcp_servers: string[];
  /** Skills active in this session. */
  skills: string[];
  permission_mode: string;
}

export interface AgentSessionInfo {
  session_id: string;
  name: string;
  state: 'idle' | 'running' | 'paused' | 'completed' | 'failed';
  model: string;
  workspace: string;
  created_at: string;
  last_activity?: string;
  is_pinned: boolean;
  source: string;
  /** MCP servers active in this session (populated when session_activated event is received). */
  mcp_servers?: string[];
  /** Skills active in this session (populated when session_activated event is received). */
  skills?: string[];
  /** Permission mode (populated when session_activated event is received). */
  permission_mode?: string;
}

export interface ChatAdversarialRun {
  id: string;
  agent_task_id?: string | null;
  session_id?: string | null;
  thread_id?: string | null;
  thread_title?: string | null;
  thread_pinned?: boolean;
  parent_run_id?: string | null;
  iteration_no: number;
  question: string;
  models: string[];
  judge_model?: string | null;
  status: 'queued' | 'running' | 'completed' | 'failed' | string;
  current_round: number;
  max_rounds: number;
  winner_model?: string | null;
  winner_reason?: string | null;
  final_answer?: string | null;
  error_message?: string | null;
  trace?: Record<string, unknown> | null;
  created_at: string;
  updated_at: string;
  completed_at?: string | null;
}

export interface ChatAdversarialStreamEvent {
  seq: number;
  runId: string;
  threadId?: string | null;
  round?: number | null;
  phase: 'initial' | 'review' | 'judge' | 'final' | 'system' | string;
  model?: string | null;
  messageId: string;
  event:
    | 'snapshot'
    | 'run_queued'
    | 'run_started'
    | 'round_started'
    | 'round_completed'
    | 'model_started'
    | 'model_delta'
    | 'model_completed'
    | 'model_failed'
    | 'model_cancelled'
    | 'judge_started'
    | 'judge_delta'
    | 'judge_completed'
    | 'judge_failed'
    | 'judge_cancelled'
    | 'final_started'
    | 'final_delta'
    | 'final_completed'
    | 'final_failed'
    | 'final_cancelled'
    | 'cancel_requested'
    | 'run_completed'
    | 'run_failed'
    | 'run_cancelled'
    | string;
  delta?: string | null;
  text?: string | null;
  error?: string | null;
  status?: string | null;
  degraded?: boolean;
  usage?: Record<string, unknown> | null;
  createdAtMs?: number;
}

export interface AgentToolCall {
  index: number;
  tool_name: string;
  /** "mcp", "skill", or "builtin" */
  source: string;
  /** For MCP: the server name. For skill: the skill name. Empty for built-in. */
  source_name: string;
  input: string | Record<string, unknown>;
  output: string | Record<string, unknown>;
  is_error: boolean;
  duration_ms: number;
}

/** Metadata about the active configuration for an agent session. */
export interface SessionMetadata {
  mcp_servers: string[];
  skills: string[];
  permission_mode: string;
  model: string;
}

/** Emitted when the session's runtime was hot-reloaded (MCP/Skills changed). */
export interface ConfigHotReloadEvent {
  type: 'config_hot_reload';
  mcp_servers: string[];
  skills: string[];
  permission_mode: string;
  model: string;
}

/** Emitted when the session was auto-compacted.
 * Mirrors the Rust `SessionCompactedEvent` (`routes::super_assistant`).
 * `summary_tokens` / `retained_tail_tokens` are optional field-completeness
 * extensions used for Zero_Loss reconciliation (design Property 7 / Req 4.2);
 * they are omitted when unknown. */
export interface SessionCompactedEvent {
  type: 'session_compacted';
  removed_messages: number;
  summary: string;
  /** Token count of the generated summary (Zero_Loss reconciliation). */
  summary_tokens?: number;
  /** Tokens retained at the tail after compaction. */
  retained_tail_tokens?: number;
}

export interface AgentUsage {
  input_tokens: number;
  output_tokens: number;
  cache_creation_tokens: number;
  cache_read_tokens: number;
  total_tokens: number;
  estimated_cost_usd: number;
  model: string;
}

export interface AgentTurnResult {
  session_id: string;
  text: string;
  tool_calls: AgentToolCall[];
  usage: AgentUsage;
  iterations: number;
  compacted?: {
    summary: string;
    removed_messages: number;
    count: number;
  } | null;
  metadata?: SessionMetadata;
  pm_quality?: Record<string, unknown>;
  pm_report?: {
    schemaVersion?: string;
    questionType?: string;
    quantEnabled?: boolean;
    reportJson?: Record<string, unknown>;
    reportHtml?: string;
  };
}

export interface AgentToolCallBlock {
  id: string;
  name: string;
  input: string;
  /** Tool execution result — populated when tool results are returned separately. */
  result?: AgentToolResultBlock;
}

export interface AgentToolResultBlock {
  tool_use_id: string;
  tool_name: string;
  output: string;
  is_error: boolean;
}

export interface AgentMessageUsage {
  input_tokens: number;
  output_tokens: number;
  cache_creation_input_tokens: number;
  cache_read_input_tokens: number;
}

export interface AgentHistoryMessage {
  role: 'user' | 'assistant' | 'tool' | 'system';
  content: string;
  tool_calls?: AgentToolCallBlock[];
  tool_result?: AgentToolResultBlock;
  usage?: AgentMessageUsage;
  /** Full thinking/reasoning content from extended thinking blocks (e.g. Claude internal reasoning). */
  thinking?: string | null;
  /** PM background task id bound to this assistant message (if any). */
  pm_task_id?: string | null;
  /** PM background task terminal status (if any). */
  pm_task_status?: string | null;
}

export interface AgentSessionHistory {
  session_id: string;
  messages: AgentHistoryMessage[];
  page?: AgentSessionHistoryPage | null;
  pm_research?: PmSessionHistoryReplay | null;
  super_assistant_turns?: SuperAssistantTurnMessageMetadata[] | null;
}

export interface SuperAssistantTurnMessageMetadata {
  turn_id: string;
  model: string;
  final_text: string;
  route_capability?: string | null;
  adversarial_run_id?: string | null;
  judge_model?: string | null;
  winner_model?: string | null;
  winner_reason?: string | null;
  attribution_task_id?: string | null;
  nl2sql_audits?: SuperAssistantNl2sqlAudit[];
  completed_at?: string | null;
}

export interface SuperAssistantNl2sqlAudit {
  tool_call_id: string;
  status: string;
  input?: unknown;
  result?: unknown;
  error_message?: string | null;
}

export interface AgentSessionHistoryPage {
  before_turn_cursor: number;
  next_before_turn_cursor?: number | null;
  has_more: boolean;
  returned_turns: number;
  total_turns: number;
  limit_turns: number;
  max_bytes: number;
  approx_payload_bytes: number;
}

export interface PmSessionHistoryReplayEvent {
  task_id: string;
  session_id: string;
  status: string;
  stage?: string | null;
  attempt?: number | null;
  message?: string | null;
  elapsed_ms: number;
  stage_elapsed_ms?: number | null;
  detail?: Record<string, unknown> | null;
  response?: Record<string, unknown> | null;
  error?: string | null;
}

export interface PmSessionHistoryReplay {
  task_id: string;
  status: string;
  events: PmSessionHistoryReplayEvent[];
}

// Tenant types

export interface TenantInfo {
  id: string;
  name: string;
  slug: string;
  plan: string;
  max_users?: number;
  max_tokens_monthly?: number;
  is_system: boolean;
  created_at: string;
  updated_at: string;
  user_count?: number;
  /** Current monthly token usage (queried from token_usage table). */
  usage_this_month?: number;
  /** Current user count this month. */
  current_users?: number;
}

export interface TenantListResponse {
  tenants: TenantInfo[];
  total: number;
  page: number;
  per_page: number;
}

// Command types

export interface CommandInfo {
  name: string;
  description: string;
  hint?: string;
  source?: 'skill' | 'builtin';
}

// =============================================================================
// Gitlab Project types
// =============================================================================

export interface GitlabProject {
  id: string;
  name: string;
  url: string;
  branch: string;
  description?: string;
  is_cloned: boolean;
  clone_path?: string;
  last_sync_at?: string;
  created_at: string;
}

// =============================================================================
// Data Source types — multi-tenant data source registry
// =============================================================================

export type DataSourceDbType =
  | 'mysql'
  | 'tidb'
  | 'postgres'
  | 'clickhouse'
  | 'presto'
  | 'trino'
  | 'mongodb';

export type DataSourceVisibility = 'tenant' | 'private';

export interface DataSourceConfigSql {
  uri?: string;
  host: string;
  port: number;
  database: string;
  catalog?: string;
  schema?: string;
  schemas?: string[];
  username: string;
  password: string;
  ssl?: boolean;
  basic_auth?: boolean;
  auth_source?: string;
  tls?: boolean;
  extra_params?: Record<string, string>;
}

export interface DataSourceColumn {
  name: string;
  type: string;
  description?: string;
  nullable?: boolean;
  primary_key?: boolean;
}

export interface DataSourceSchemaInfo {
  table_name: string;
  name?: string;
  physical_table_name?: string;
  catalog?: string;
  schema?: string;
  qualified_name?: string;
  fully_qualified_name?: string;
  /** Table-level description (from nl2sql_table_desc_semantics). */
  description?: string;
  /** Whether this table was manually created (not discovered from DB). */
  is_manual?: boolean;
  columns: DataSourceColumn[];
}

export interface DataSourceInfo {
  id: string;
  tenant_id: string;
  user_id: string | null;
  name: string;
  description: string | null;
  db_type: DataSourceDbType;
  visibility: DataSourceVisibility;
  config_preview: Record<string, unknown>;
  config_plain?: Record<string, unknown>;
  schema_info: { tables: DataSourceSchemaInfo[]; foreign_keys?: unknown[] } | null;
  enabled: boolean;
  last_tested_at: string | null;
  last_error: string | null;
  created_by: string | null;
  created_at: string;
  updated_at: string;
  sensitive_columns: string[] | null;
  embedding_status: 'not_started' | 'building' | 'built' | 'failed';
}

export interface DataSourceListResponse {
  data_sources: DataSourceInfo[];
  total: number;
}

export interface TestConnectionResult {
  success: boolean;
  latency_ms: number;
  error?: string;
  schema_preview?: Record<string, unknown>;
}

// =============================================================================
// NL2SQL types
// =============================================================================

export interface Nl2sqlQueryRequest {
  data_source_id: string;
  question: string;
  conversation_id?: string;
  route_confidence?: number;
  routing_method?: string;
  semantic_context?: Record<string, unknown> | unknown[];
  reference_bindings?: Nl2sqlReferenceBindings;
}

export interface Nl2sqlQueryResponse {
  sql: string | null;
  explanation: string | null;
  error: string | null;
  queryId: string;
  conversationId: string | null;
  summaryVersion?: number | null;
  clarificationQuestion?: string | null;
  confirmedRequirements?: string[] | null;
  missingRequirements?: string[] | null;
  appliedRules?: AppliedRuleHit[];
  usedReferences?: Nl2sqlReferenceUsage[];
}

export interface AppliedRuleHit {
  ruleKey: string;
  ruleName: string;
  detail?: string | null;
}

export interface Nl2sqlReferenceBindings {
  packIds?: string[];
  fileIds?: string[];
  includeAll?: boolean;
}

export interface Nl2sqlReferenceFile {
  id: string;
  packId: string;
  datasourceId: string;
  filename: string;
  mediaType?: string | null;
  language?: string | null;
  sizeBytes: number;
  contentHash: string;
  status: string;
  error?: string | null;
  summary?: string | null;
  versionNo?: number;
  metadata?: Record<string, unknown> | null;
  chunkCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface Nl2sqlReferencePack {
  id: string;
  datasourceId: string;
  datasourceBindings?: string[];
  name: string;
  description?: string | null;
  scope: string;
  tags: string[];
  enabled: boolean;
  verified?: boolean;
  stale?: boolean;
  knowledgeKind?: string;
  metadata?: Record<string, unknown> | null;
  fileCount: number;
  chunkCount: number;
  writable: boolean;
  files: Nl2sqlReferenceFile[];
  createdAt: string;
  updatedAt: string;
}

export interface Nl2sqlReferenceUsage {
  packId: string;
  packName: string;
  fileId: string;
  filename: string;
  chunkId: string;
  language?: string | null;
  startLine: number;
  endLine: number;
  score: number;
  reason: string;
  chunkType?: string;
  verified?: boolean;
  stale?: boolean;
  preview: string;
}

export interface SqlKnowledgeSearchResponse {
  references: Nl2sqlReferenceUsage[];
}

export interface SqlKnowledgeReadResponse {
  fileId: string;
  filename: string;
  startLine: number;
  endLine: number;
  content: string;
}

export interface SqlKnowledgeImportTask {
  id: string;
  packId: string;
  datasourceId: string;
  status: 'pending' | 'running' | 'completed' | 'partial' | 'timed_out' | 'failed';
  totalFiles: number;
  processedFiles: number;
  failedFiles: number;
  currentFilename?: string | null;
  errorMessage?: string | null;
  failureDetails: Array<{ filename?: string; error?: string }>;
  createdAt: string;
  startedAt?: string | null;
  completedAt?: string | null;
  updatedAt: string;
}

export interface Nl2sqlQueryTaskStartResponse {
  taskId: string;
  status: string;
}

export interface Nl2sqlQueryTaskEvent {
  task_id: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsed_ms: number;
  stage_elapsed_ms?: number | null;
  response?: Nl2sqlQueryResponse | null;
  error?: string | null;
}

export interface Nl2sqlQueryTaskStatusResponse {
  taskId: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsedMs: number;
  stageElapsedMs?: number | null;
  response?: Nl2sqlQueryResponse | null;
  error?: string | null;
}

export interface Nl2sqlExecuteRequest {
  query_id: string;
  sql: string;
  data_source_id: string;
}

export interface Nl2sqlColumn {
  name: string;
  type: string;
}

export interface Nl2sqlExecuteResponse {
  columns: Nl2sqlColumn[];
  rows: Record<string, unknown>[];
  rows_count: number;
  total_rows: number;
  has_more: boolean;
  limit: number;
  offset: number;
  execution_ms: number;
  error: string | null;
  result_score?: number | null;
  warnings?: ValidationWarning[] | null;
  suggestions?: string[] | null;
  applied_rules?: AppliedRuleHit[];
}

export interface Nl2sqlColumnNote {
  column: string;
  observation: string;
}

export interface Nl2sqlExplainResponse {
  explanation: string;
  summary: string;
  insights?: string[];
  actions?: string[];
  risks?: string[];
  chart_recommendation?: string | null;
  column_notes?: Nl2sqlColumnNote[];
}

// F-10: Query Permission Policy
export interface Nl2sqlQueryPolicy {
  id: number;
  tenant_id: string;
  datasource_id: string;
  user_id: string;
  user_name: string | null;
  user_email: string | null;
  allowed_tables: string[];
  denied_tables: string[];
  allowed_columns: string[];
  denied_columns: string[];
  row_filter_expr: string | null;
  description: string | null;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface Nl2sqlQueryPolicyListResponse {
  items: Nl2sqlQueryPolicy[];
  total: number;
}

// F-11: Slow query analytics
export interface SlowQueryItem {
  id: string;
  question: string;
  dataSourceId: string;
  generatedSql: string | null;
  executionMs: number;
  rowsReturned: number | null;
  createdAt: string;
}

export interface SlowQueriesResponse {
  items: SlowQueryItem[];
  total: number;
  p50Ms: number | null;
  p95Ms: number | null;
  p99Ms: number | null;
}

export interface Nl2sqlQueryHistoryItem {
  id: string;
  data_source_id: string | null;
  question: string;
  generated_sql: string | null;
  executed: boolean;
  rows_returned: number;
  execution_ms: number;
  error_message: string | null;
  created_at: string;
}

export interface Nl2sqlQueryHistoryResponse {
  queries: Nl2sqlQueryHistoryItem[];
  total: number;
}

export interface RefreshTaskFailure {
  table: string;
  error: string;
}

export interface RefreshTaskStatus {
  task_id: string;
  datasource_id: string;
  status: string;
  progress: number;
  processed_tables: number;
  error_message: string | null;
  /** Per-table failures. Present only when the refresh completed with a
   *  partial failure. */
  failed_tables?: RefreshTaskFailure[] | null;
  completed_at: string | null;
}

export interface RefreshTaskListItem extends RefreshTaskStatus {
  datasource_name: string;
  trigger_source: string;
  total_tables: number;
  created_at: string;
  updated_at: string;
}

export interface RefreshTaskListResponse {
  items: RefreshTaskListItem[];
}

// ── NL2SQL Semantics types ──────────────────────────────────────────────────

export interface TableSemantics {
  table_name: string;
  embedding_model: string;
  /** AI-generated description (editable by user; backend will polish and re-embed). */
  ai_description: string;
  is_indexed: boolean;
  version: number;
}

export interface DatasourceSemantics {
  embedding_model: string;
  ai_description: string;
  user_description?: string | null;
  is_indexed: boolean;
  version: number;
}

export interface UpdateTableDescriptionRequest {
  description: string;
}

export interface UpdateDatasourceDescriptionRequest {
  description: string;
}

export interface UpdateSemanticsResponse {
  success: boolean;
}

// ── Multi-turn Clarification ───────────────────────────────────────────────────

export interface ClarifyOption {
  option_index: number;
  data_source_id: string;
  table_name: string;
  column_name: string;
  reason: string;
  sim_score: number;
  business_meaning?: string;
}

export interface ClarificationContext {
  original_question: string;
  clarification_question: string;
  options: ClarifyOption[];
  confirmed_requirements?: string[];
  missing_requirements?: string[];
  missing_requirement_reasons?: Array<{
    key: string;
    requirement: string;
    why_missing: string;
    how_to_provide: string;
    examples?: string[];
  }>;
  clarification_history?: Array<{
    round: number;
    user_input: string;
    missing_after?: string[];
  }>;
  turn: number;
  conversation_id: string;
}

export interface SelectedOption {
  option_index: number;
}

export interface ClarifyRequest {
  session_id: string;
  /** Optional conversation ID for multi-turn context. */
  conversation_id?: string;
  /** The original question. */
  question?: string;
  clarification_context?: ClarificationContext;
  selected_option?: SelectedOption;
  free_text?: string;
  route_confidence?: number;
  routing_method?: string;
  semantic_context?: Record<string, unknown> | unknown[];
  /** Original async query task waiting for this clarification. */
  source_query_task_id?: string;
}

export interface ClarifyResponse {
  data: {
    query_id: string;
    data_source_id: string;
    question: string;
    sql: string | null;
    explanation?: string | null;
    error: string | null;
    execution_result?: {
      columns: string[];
      rows: Record<string, unknown>[];
      rows_count: number;
      execution_ms: number;
    } | null;
    clarification_context?: ClarificationContext | null;
    fallback_mode?: string | null;
    conversation_id?: string | null;
    summary_version?: number | null;
    applied_rules?: AppliedRuleHit[];
  } | null;
  pending_clarification?: ClarificationContext | null;
  error?: string | null;
}

export interface ClarifyTaskStartResponse {
  taskId: string;
  status: string;
}

export interface ClarifyTaskEvent {
  task_id: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsed_ms: number;
  stage_elapsed_ms?: number | null;
  response?: ClarifyResponse | null;
  error?: string | null;
}

export interface ClarifyTaskStatusResponse {
  taskId: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsedMs: number;
  stageElapsedMs?: number | null;
  response?: ClarifyResponse | null;
  error?: string | null;
}

export interface ClarifyPendingResponse {
  pending_clarification: ClarificationContext | null;
}

// ── Route Response ────────────────────────────────────────────────────────────

export interface RouteResponse {
  routed: boolean;
  result: RouteResult | null;
  error: string | null;
}

export interface RouteTaskStartResponse {
  taskId: string;
  status: string;
}

export interface RouteTaskEvent {
  task_id: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsed_ms: number;
  stage_elapsed_ms?: number | null;
  response?: RouteResponse | null;
  error?: string | null;
}

export interface RouteTaskStatusResponse {
  taskId: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsedMs: number;
  stageElapsedMs?: number | null;
  response?: RouteResponse | null;
  error?: string | null;
}

export interface RouteResult {
  data_source_id: string;
  confidence: number;
  method: string;
  matched_tables: MatchedTable[];
  clarification_question?: string | null;
}

export interface MatchedTable {
  data_source_id: string;
  table_name: string;
  best_column: string;
  column_description: string;
  similarity_score: number;
}

// ── Manual table/column management types ───────────────────────────────────

export interface ManualColumn {
  name: string;
  type: string;
  description?: string;
  nullable?: boolean;
}

export interface ManualTable {
  table_name: string;
  description?: string;
  is_manual: boolean;
  columns: ManualColumn[];
}

export interface AddManualTableRequest {
  table_name: string;
  description?: string;
  columns: ManualColumn[];
}

export interface PutManualTableRequest {
  table_name?: string;
  description?: string;
}

export interface AddManualColumnRequest {
  name: string;
  type: string;
  description?: string;
  nullable?: boolean;
}

export interface PutManualColumnRequest {
  name?: string;
  type?: string;
  description?: string;
  nullable?: boolean;
}

// ── NL2SQL Manual Foreign Keys types ────────────────────────────────────────

export interface CreateForeignKeyRequest {
  sourceTable: string;
  sourceColumn: string;
  sourceType: string;
  targetTable: string;
  targetColumn: string;
  targetType: string;
}

export interface ForeignKeyResponse {
  id: string;
  datasourceId: string;
  sourceTable: string;
  sourceColumn: string;
  sourceType: string;
  targetTable: string;
  targetColumn: string;
  targetType: string;
  createdBy: string | null;
  updatedBy: string | null;
  createdAt: string;
}

export interface ForeignKeyListResponse {
  foreignKeys: ForeignKeyResponse[];
}

// ── Multi-datasource Agent (P0-2) ─────────────────────────────────────────────

export interface StepExecutionDetail {
  step_id: number;
  step_type: string;
  datasource_id: string | null;
  description: string;
  output_name: string;
  sql?: string | null;
  columns: string[];
  rows: Record<string, unknown>[];
  row_count: number;
  execution_ms: number;
  error: string | null;
}

export interface AgentExecuteResponse {
  steps: StepExecutionDetail[];
  final_result: {
    columns: string[];
    rows: Record<string, unknown>[];
    row_count: number;
    description: string;
  } | null;
  total_execution_ms: number;
  total_steps: number;
  usedReferences?: Nl2sqlReferenceUsage[];
  conversationId?: string | null;
  queryId?: string | null;
  error: string | null;
}

export interface AgentTaskStartResponse {
  taskId: string;
  status: string;
}

export interface AgentTaskEvent {
  task_id: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsed_ms: number;
  stage_elapsed_ms?: number | null;
  response?: AgentExecuteResponse | null;
  error?: string | null;
}

export interface AgentTaskStatusResponse {
  taskId: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsedMs: number;
  stageElapsedMs?: number | null;
  response?: AgentExecuteResponse | null;
  error?: string | null;
}

// ── Data Attribution ─────────────────────────────────────────────────────────

export type AttributionDepth = 'fast' | 'standard' | 'deep';

export interface AttributionAnalyzeRequest {
  question: string;
  conversation_id?: string;
  datasource_ids?: string[];
  depth?: AttributionDepth;
}

export interface AttributionPlanStep {
  id: string;
  title: string;
  purpose: string;
  question: string;
  priority?: number;
}

export interface AttributionPlan {
  needsClarification?: boolean;
  clarificationQuestion?: string | null;
  confidence?: number | null;
  analysisFocus?: string[];
  steps?: AttributionPlanStep[];
}

export interface AttributionObservation {
  stepId: string;
  title: string;
  purpose: string;
  question: string;
  datasourceIds?: string[];
  timeContext?: string | null;
  queryId?: string | null;
  conversationId?: string | null;
  columns: string[];
  rows: Record<string, unknown>[];
  rowCount: number;
  sampled?: boolean;
  sqls: string[];
  usedReferences?: Nl2sqlReferenceUsage[];
  error?: string | null;
  elapsedMs: number;
}

export interface AttributionDriver {
  title: string;
  explanation: string;
  impact?: string | null;
  evidenceStepIds?: string[];
  confidence?: string | null;
}

export interface AttributionReport {
  title: string;
  executiveSummary: string;
  metricAnswer?: string | null;
  mainCauses?: AttributionDriver[];
  recommendations?: string[];
  caveats?: string[];
  nextQuestions?: string[];
  confidence?: string | null;
  coverage?: string | null;
}

export interface AttributionEvidenceHealth {
  totalSteps: number;
  executionSucceededSteps?: number;
  usableEvidenceSteps?: number;
  zeroRowSteps?: number;
  successfulSteps: number;
  failedSteps: number;
  sampledSteps: number;
  totalRows: number;
}

export interface AttributionAnalyzeResponse {
  status: string;
  question: string;
  depth: string;
  conversationId?: string | null;
  clarificationQuestion?: string | null;
  report?: AttributionReport | null;
  plan?: AttributionPlan | null;
  observations: AttributionObservation[];
  evidenceHealth: AttributionEvidenceHealth;
  evidenceCards?: AttributionEvidenceCard[];
  totalExecutionMs: number;
  error?: string | null;
}

export interface AttributionTaskStartResponse {
  taskId: string;
  status: string;
  conversationId: string;
}

export interface AttributionTaskEvent {
  task_id: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsed_ms: number;
  stage_elapsed_ms?: number | null;
  progress_percent?: number | null;
  step_index?: number | null;
  step_total?: number | null;
  observation?: AttributionObservation | null;
  response?: AttributionAnalyzeResponse | null;
  error?: string | null;
}

export interface AttributionTaskStatusResponse {
  taskId: string;
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsedMs: number;
  stageElapsedMs?: number | null;
  progressPercent?: number | null;
  stepIndex?: number | null;
  stepTotal?: number | null;
  observation?: AttributionObservation | null;
  response?: AttributionAnalyzeResponse | null;
  error?: string | null;
}

export interface AttributionEvidenceCard {
  stepId: string;
  title: string;
  purpose: string;
  question: string;
  datasourceIds?: string[];
  timeContext?: string | null;
  status: string;
  rowCount: number;
  sampled?: boolean;
  columns?: string[];
  rowsPreview?: Record<string, unknown>[];
  numericHighlights?: string[];
  sqlCount?: number;
  referenceFiles?: string[];
  error?: string | null;
  evidenceRefs?: Array<{
    rowIndex: number;
    column: string;
    valuePreview: string;
  }>;
}

export interface AttributionConversationItem {
  id: string;
  messageCount: number;
  summary?: string | null;
  lastQuestion?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface AttributionConversationListResponse {
  total: number;
  conversations: AttributionConversationItem[];
}

export interface AttributionConversationTaskItem {
  taskId: string;
  conversationId: string;
  question: string;
  depth: string;
  status: string;
  summary?: string | null;
  response?: AttributionAnalyzeResponse | null;
  error?: string | null;
  totalExecutionMs: number;
  createdAt: string;
  updatedAt: string;
}

export interface AttributionConversationDetailResponse {
  id: string;
  messageCount: number;
  summary?: string | null;
  lastQuestion?: string | null;
  tasks: AttributionConversationTaskItem[];
  createdAt: string;
  updatedAt: string;
}

// ── Conversation Summary (P3-2) ───────────────────────────────────────────────

export interface ConversationMessage {
  message_type?: 'query' | 'clarification' | string;
  query_id: string;
  data_source_id: string | null;
  question: string;
  generated_sql: string | null;
  rows_returned: number | null;
  execution_ms: number | null;
  created_at: string;
  clarification_turn?: number | null;
  clarification_question?: string | null;
  clarification_answer?: string | null;
  confirmed_requirements?: string[] | null;
  missing_requirements?: string[] | null;
  applied_rules?: AppliedRuleHit[] | null;
  used_references?: Nl2sqlReferenceUsage[] | null;
}

export interface ConversationItem {
  id: string;
  message_count: number;
  summary: string | null;
  last_question: string | null;
  created_at: string;
  updated_at: string;
}

export interface ConversationDetail {
  id: string;
  message_count: number;
  total_messages: number;
  page: number;
  per_page: number;
  has_more: boolean;
  summary: string | null;
  last_question: string | null;
  messages: ConversationMessage[];
  created_at: string;
  updated_at: string;
}

export interface ConversationListResponse {
  conversations: ConversationItem[];
  total: number;
}

export interface PatchConversationRequest {
  summary?: string;
  regenerate_summary?: boolean;
}

export interface DeleteConversationResponse {
  deleted: boolean;
  id: string;
}

// =============================================================================
// NL2SQL Enterprise: Business Domains
// =============================================================================

export interface BusinessDomain {
  id: number;
  datasourceId: string;
  domainName: string;
  domainDescription: string;
  tableCount: number;
  confidenceScore: number;
  source: 'auto' | 'manual';
  domainRoutingMode?: 'assist' | 'strict';
  tables: string[];
}

export interface ListBusinessDomainsResponse {
  domains: BusinessDomain[];
}

export interface ListDomainsForDatasourceResponse {
  domains: BusinessDomain[];
}

export interface CreateDomainRequest {
  name: string;
  description?: string;
  tableNames?: string[];
  domainRoutingMode?: 'assist' | 'strict';
}

export interface CreateDomainResponse {
  id: number;
  datasourceId: string;
  domainName: string;
  domainDescription?: string;
  tableCount: number;
  confidenceScore: number;
  source: 'auto' | 'manual';
  domainRoutingMode?: 'assist' | 'strict';
  tables: string[];
}

export interface UpdateDomainRequest {
  domainName: string;
  domainDescription?: string;
  domainRoutingMode?: 'assist' | 'strict';
}

export interface RediscoverDomainsResponse {
  domainsDiscovered: number;
}

// =============================================================================
// NL2SQL Enterprise: Schema Change Notifications
// =============================================================================

export interface SchemaChangeNotification {
  id: number;
  datasourceId: string;
  changeType: 'tables_added' | 'tables_removed' | 'tables_changed' | 'columns_added' | 'columns_removed' | 'columns_changed' | 'types_changed';
  details: SchemaChangeDetailItem[];
  recommendedAction: 'reindex' | 'review_semantics' | 'no_action';
  status: 'pending' | 'approved' | 'rejected' | 'completed';
  affectedQueriesCount: number;
  createdAt: string;
  reviewedBy: string;
  reviewedAt?: string;
}

export interface SchemaChangeDetailItem {
  table?: string;
  column?: string;
  oldValue?: string;
  newValue?: string;
}

export interface AffectedQuery {
  queryId: string;
  question?: string;
  generatedSql?: string;
  impactLevel: 'high' | 'medium' | 'low';
}

export interface SchemaChangeDetailResponse extends SchemaChangeNotification {
  affectedQueries: AffectedQuery[];
}

export interface ListSchemaChangesResponse {
  changes: SchemaChangeNotification[];
  total: number;
}

// =============================================================================
// NL2SQL Enterprise: Time Patterns
// =============================================================================

export interface TimePattern {
  id: number;
  patternRegex: string;
  patternDisplay: string;
  resolvedType: 'today' | 'yesterday' | 'this_week' | 'this_month' | 'last_month' | 'this_quarter' | 'last_quarter' | 'this_year' | 'ytd' | 'mom' | 'yoy' | 'wow' | 'woww' | 'qoq' | 'custom';
  granularity: 'day' | 'week' | 'month' | 'quarter' | 'year';
  offsetDays: number;
  priority: number;
  enabled: boolean;
}

export interface ListTimePatternsResponse {
  patterns: TimePattern[];
}

export interface CreateTimePatternRequest {
  patternRegex: string;
  patternDisplay?: string;
  resolvedType: string;
  granularity?: string;
  offsetDays?: number;
  priority?: number;
  testText?: string;
}

export interface UpdateTimePatternRequest {
  patternRegex?: string;
  patternDisplay?: string;
  resolvedType?: string;
  granularity?: string;
  offsetDays?: number;
  priority?: number;
  enabled?: boolean;
  testText?: string;
}

// =============================================================================
// NL2SQL Enterprise: Validation Rules
// =============================================================================

export interface ValidationRule {
  id: number;
  tableName: string;
  columnName: string;
  ruleType: 'range' | 'null_ratio' | 'row_count' | 'freshness' | 'cardinality';
  ruleConfig: Record<string, unknown>;
  severity: 'warning' | 'error';
  description: string;
  enabled: boolean;
}

export interface ListValidationRulesResponse {
  rules: ValidationRule[];
}

export interface CreateValidationRuleRequest {
  tableName: string;
  columnName: string;
  ruleType: string;
  ruleConfig: Record<string, unknown>;
  severity?: string;
  description?: string;
}

export interface UpdateValidationRuleRequest {
  tableName?: string;
  columnName?: string;
  ruleType?: string;
  ruleConfig?: Record<string, unknown>;
  severity?: string;
  description?: string;
  enabled?: boolean;
}

// =============================================================================
// NL2SQL Enterprise: Column Masking Rules (R-7 / R-8)
// =============================================================================

export type MaskingRuleType = 'redact' | 'hash' | 'tokenize' | 'partial' | 'null' | 'constant';

/** A column-level masking rule. `null` datasource_id means "all datasources in tenant". */
export interface MaskingRule {
  id: number;
  datasource_id: string | null;
  table_name: string;
  column_name: string;
  mask_type: MaskingRuleType;
  pattern: string | null;
  constant_value: string | null;
  priority: number;
  description: string | null;
  enabled: boolean;
}

export interface ListMaskingRulesResponse {
  rules: MaskingRule[];
}

export interface CreateMaskingRuleRequest {
  datasource_id?: string | null;
  table_name: string;
  column_name: string;
  mask_type: MaskingRuleType;
  pattern?: string | null;
  constant_value?: string | null;
  priority?: number;
  description?: string | null;
  enabled?: boolean;
}

export interface UpdateMaskingRuleRequest {
  table_name?: string;
  column_name?: string;
  mask_type?: MaskingRuleType;
  pattern?: string | null;
  constant_value?: string | null;
  priority?: number;
  description?: string | null;
  enabled?: boolean;
}

// =============================================================================
// NL2SQL Enterprise: Query Understanding
// =============================================================================

export interface QUEntities {
  time?: {
    raw: string;
    resolvedType: string;
    granularity: string;
    ranges: [string, string][];
  };
  subject?: {
    tables: string[];
    columns: string[];
    raw: string;
  };
  filters: Array<{ column: string; value: string; op: string; raw: string }>;
  aggregations: string[];
  comparisons: Array<{ type: string; raw: string }>;
}

export interface QueryUnderstandingResponse {
  rewrittenQuestion: string;
  intent: string;
  entities: QUEntities;
  confidence: number;
}

export interface ValidationWarning {
  table: string;
  column: string;
  ruleType: string;
  severity: 'warning' | 'error';
  message: string;
  actualValue: string;
  expected: string;
}

// ── NL2SQL Synonyms ───────────────────────────────────────────────────────────

export interface SynonymItem {
  id: number;
  term: string;
  canonicalTable: string;
  canonicalColumn: string;
  termType: string;
  createdBy: string | null;
  createdAt: string;
}

export interface ListSynonymsResponse {
  synonyms: SynonymItem[];
}

export interface PaginatedSynonymsResponse {
  data: SynonymItem[];
  total: number;
  page: number;
  perPage: number;
  totalPages: number;
}

export interface CreateSynonymRequest {
  term: string;
  canonicalTable: string;
  canonicalColumn: string;
  termType?: string;
}

export interface UpdateSynonymRequest {
  term?: string;
  canonicalTable?: string;
  canonicalColumn?: string;
  termType?: string;
}

// ── NL2SQL Metrics ────────────────────────────────────────────────────────────

export interface MetricItem {
  id: number;
  metricName: string;
  metricAliases: string[];
  expression: string;
  filterConditions: unknown | null;
  description: string | null;
  granularity: string;
  createdBy: string | null;
  createdAt: string;
  status?: string | null;
}

export interface ListMetricsResponse {
  metrics: MetricItem[];
}

export interface CreateMetricRequest {
  metricName: string;
  metricAliases?: string[];
  expression: string;
  filterConditions?: unknown;
  description?: string;
  granularity?: string;
}

export interface UpdateMetricRequest {
  metricName?: string;
  metricAliases?: string[];
  expression?: string;
  filterConditions?: unknown;
  description?: string;
  granularity?: string;
}

// ── NL2SQL Join Paths ────────────────────────────────────────────────────────

export interface JoinPathItem {
  id: number;
  path: string[];
  joinColumns: string[];
  dsIds: string[];
  totalColumns: number;
  verified: boolean;
  confidence: number;
  source: string;
  createdAt: string;
  // Raw DB fields (populated by create/update responses and CRUD operations).
  sourceTable?: string;
  targetTable?: string;
  sourceColumn?: string;
  targetColumn?: string;
  joinType?: string;
  pathText?: string;
  sqlJoins?: string;
  notes?: string;
}

export interface CreateJoinPathRequest {
  sourceTable: string;
  targetTable: string;
  sourceColumn: string;
  targetColumn: string;
  joinType?: string;
  confidence?: number;
  notes?: string;
}

export interface UpdateJoinPathRequest {
  sourceTable?: string;
  targetTable?: string;
  sourceColumn?: string;
  targetColumn?: string;
  joinType?: string;
  confidence?: number;
  verified?: boolean;
  notes?: string;
}

export interface ListJoinPathsResponse {
  paths: JoinPathItem[];
}

export interface RediscoverJoinPathsResponse {
  pathsDiscovered: number;
  pathsVisible: number;
}

// ── NL2SQL Cross-Datasource Relations ───────────────────────────────────────

export interface CrossDSRelationItem {
  id: number;
  leftDatasource: string;
  leftTable: string;
  leftColumn: string;
  rightDatasource: string;
  rightTable: string;
  rightColumn: string;
  matchType: string;
  confidence: number;
  verified: boolean;
  source: string;
  createdAt: string;
}

export interface ListCrossDSRelationsResponse {
  relations: CrossDSRelationItem[];
}

export interface CreateCrossDSRelationRequest {
  leftDatasource: string;
  leftTable: string;
  leftColumn: string;
  rightDatasource: string;
  rightTable: string;
  rightColumn: string;
  matchType?: string;
}

export interface UpdateCrossDSRelationRequest {
  leftDatasource?: string;
  leftTable?: string;
  leftColumn?: string;
  rightDatasource?: string;
  rightTable?: string;
  rightColumn?: string;
  matchType?: string;
  verified?: boolean;
}

// ── NL2SQL Cross-Domain Clusters ───────────────────────────────────────────

export interface CrossDomainClusterItem {
  id: number;
  clusterName: string;
  description: string | null;
  datasourceIds: string[];
  domainIds: string[];
  autoDiscovered: boolean | number;
  /// Tables derived from datasourceIds (populated by the frontend for display purposes).
  tables?: string[];
  createdBy: string | null;
  createdAt: string;
}

export interface ListCrossDomainClustersResponse {
  clusters: CrossDomainClusterItem[];
}

export interface CreateCrossDomainClusterRequest {
  clusterName: string;
  description?: string;
  datasourceIds: string[];
  tables: string[];
}

export interface UpdateCrossDomainClusterRequest {
  clusterName?: string;
  description?: string;
  datasourceIds?: string[];
  tables?: string[];
}

// ── NL2SQL Analytics ─────────────────────────────────────────────────────────
// Backend uses snake_case JSON; these types match the actual API response shapes.

export interface AnalyticsOverview {
  total_queries: number;
  success_rate: number;
  avg_route_confidence: number;
  avg_planning_ms: number;
  avg_execution_ms: number;
  planning_execution_ratio: number;
  cache_hit_queries: number;
  cache_hit_rate: number;
  total_datasources: number;
  total_tables_indexed: number;
  avg_semantic_coverage: number;
  total_conversations: number;
}

export interface AnalyticsRouting {
  confidence_distribution: Array<{ range: string; count: number }>;
  method_distribution: Array<{ method: string; count: number; rate: number }>;
  top_routed_tables: Array<{ table: string; count: number }>;
  clarification_rate: number;
}

export interface DatasourceCoverage {
  datasource_id: string;
  datasource_name: string;
  total_tables: number;
  indexed_tables: number;
  total_columns: number;
  indexed_columns: number;
  table_coverage_pct: number;
  column_coverage_pct: number;
}

export interface AnalyticsSemanticCoverage {
  datasources: DatasourceCoverage[];
  total_tables: number;
  indexed_tables: number;
  total_columns: number;
  indexed_columns: number;
}

export interface DailyTrend {
  date: string;
  queries: number;
  success_rate: number;
  avg_confidence: number;
}

export interface AnalyticsTrends {
  daily: DailyTrend[];
}

export interface AnalyticsRuleHitItem {
  rule_key: string;
  rule_name: string;
  hits: number;
  queries: number;
  query_hit_rate: number;
}

export interface AnalyticsRuleHitDaily {
  date: string;
  total_queries: number;
  queries_with_hits: number;
  coverage_rate: number;
  total_hits: number;
}

export interface AnalyticsRuleHits {
  total_queries: number;
  queries_with_rule_hits: number;
  coverage_rate: number;
  total_rule_hits: number;
  top_rules: AnalyticsRuleHitItem[];
  daily: AnalyticsRuleHitDaily[];
}

export interface AnalyticsDatasourceHealthRow {
  datasource_id: string;
  datasource_name: string;
  total_queries: number;
  successful_queries: number;
  failed_queries: number;
  success_rate: number;
  avg_execution_ms: number;
  p95_execution_ms: number | null;
}

export interface AnalyticsDatasourceHealth {
  rows: AnalyticsDatasourceHealthRow[];
  total: number;
}

export interface UserLeaderboardEntry {
  user_id: string;
  total_queries: number;
  successful_queries: number;
  success_rate: number;
  avg_execution_ms: number | null;
  avg_confidence: number | null;
  rank: number;
}

export interface UserLeaderboardResponse {
  items: UserLeaderboardEntry[];
  period_days: number;
}

// Re-export monaco-editor types used by the hooks page
export type { IStandaloneCodeEditor } from 'monaco-editor';
