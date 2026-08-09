import type {
  RdAgentProfile,
  RdAgentWorkflow,
  RdFileChange,
  RdRepository,
  RdRepositoryFileSuggestion,
  RdTask,
  RdTaskEvent,
  RdStudioMode,
} from '@/types';

export type { RdStudioMode };

export type RdTaskThreadSummary = {
  threadId: string;
  title: string;
  latest: RdTask;
  tasks: RdTask[];
  count: number;
};

export type RuntimeToolCall = {
  index?: number;
  toolName?: string;
  source?: string;
  sourceName?: string;
  isError?: boolean;
  durationMs?: number;
  input?: string;
  output?: string;
  target?: string;
  reason?: string;
  attribution?: Record<string, unknown>;
  governanceSnapshot?: Record<string, unknown>;
};

export type RdTimelineEvent = RdTaskEvent & {
  displayStartedAt?: string;
  displayCompletedAt?: string;
  displayDurationMs?: number;
  displayToolTarget?: string;
};

export type RuntimeConfigSnapshot = {
  mcpServers: string[];
  skills: string[];
  permissionMode?: string | null;
};

export type RdTokenUsageRow = {
  key: string;
  stage: string;
  label: string;
  model?: string;
  inputTokens: number;
  outputTokens: number;
  cacheCreationTokens: number;
  cacheReadTokens: number;
  totalTokens: number;
};

export type RdRiskLevel = 'low' | 'medium' | 'high' | 'critical';

export type RdRiskFile = {
  path: string;
  riskLevel: RdRiskLevel;
  reasons: string[];
  signals: string[];
  lineHints: number[];
  additions: number;
  deletions: number;
};

export type RdRiskMap = {
  riskLevel: RdRiskLevel;
  mode?: string;
  sourceStage?: string;
  files: RdRiskFile[];
  summary?: Record<string, unknown>;
};

export type RdWorkspaceTabKey =
  | 'result'
  | 'file'
  | 'diff'
  | 'tests'
  | 'timeline'
  | 'context'
  | 'references'
  | 'preview'
  | 'runtime'
  | 'tokens';

export type RdSharePreviewPayload = {
  schema: 'aos-rd-task-report-v1';
  title: string;
  generatedAt: string;
  messageId: string;
  taskId?: string | null;
  content: string;
  truncated?: boolean;
};

export type RdFileMentionCandidate = RdRepositoryFileSuggestion & {
  repositoryId: string;
  repositoryName: string;
  repositoryBranch: string;
  mentionValue: string;
  isPrimaryRepository: boolean;
};

export type CodeChatPanelProps = {
  canWrite: boolean;
  isPending: boolean;
  modelOptionsCount: number;
  selectedRepo: RdRepository | null;
  workspaceRepositories: RdRepository[];
  selectedTask: RdTask | null;
  model?: string;
  deepModeEnabled: boolean;
  continueFromCurrentTask: boolean;
  selectedAgentProfile?: RdAgentProfile;
  selectedWorkflow?: RdAgentWorkflow;
  initialPrompt?: string;
  onContinueFromCurrentTaskChange: (checked: boolean) => void;
  onSubmit: (prompt: string) => Promise<unknown>;
};

export type ParsedDiffHunk = {
  index: number;
  title: string;
  lines: string[];
};
