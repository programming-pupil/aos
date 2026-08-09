// ── Shared chat types ───────────────────────────────────────────────────────────

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
}

export type ContentBlock = TextBlock | ImageBlock | DocumentBlock;

export interface ToolCallInfo {
  index: number;
  name: string;
  source: 'mcp' | 'builtin' | 'skill';
  mcpServer?: string;
  skillName?: string;
  args: string;
  result: string;
  isError: boolean;
  status: 'pending' | 'running' | 'success' | 'error';
  durationMs?: number;
}

export interface ChatEvidenceSource {
  id: string;
  type: 'web' | 'file' | 'memory';
  title: string;
  url?: string;
  fileId?: string;
  filename?: string;
  memoryId?: string;
  sessionId?: string;
  lineStart?: number;
  lineEnd?: number;
  snippet?: string;
  sourceLabel?: string;
}

export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string | ContentBlock[];
  toolCalls?: ToolCallInfo[];
  evidenceSources?: ChatEvidenceSource[];
  thinking?: string | null;
  thinkingLoading?: boolean;
  /**
   * Duration in milliseconds between the first `thinking_start` event and
   * either `thinking_end` or the first `text_delta` that closed the
   * reasoning stream. Used by the thinking bubble to render the
   * "已深度思考（Xs）" label once reasoning has completed, matching
   * the collapsible-with-duration UX popularized by DeepSeek.
   */
  thinkingDurationMs?: number;
  isStreaming?: boolean;
  isBookmarked?: boolean;
  timestamp?: number;
  createdAt?: string | number | null;
  /** For reply reference */
  replyTo?: string;
  /** Effective model that produced this assistant response. */
  modelName?: string;
  /** Super-adversarial judge model, when this response came from a debate. */
  judgeModel?: string;
  /** Winning model selected by the super-adversarial judge. */
  winnerModel?: string;
  winnerReason?: string;
  adversarialRunId?: string;
  localCommand?: boolean;
}

export interface SlashCommandDef {
  name: string;
  description: string;
  hint?: string;
  source: 'builtin' | 'skill';
  category?: string;
}

export interface SessionItem {
  sessionId: string;
  name: string;
  state: string;
  model: string;
  createdAt: string;
  lastActivity?: string;
  mcpServers?: string[];
  skills?: string[];
  permissionMode?: string;
  isPinned?: boolean;
  isBookmarked?: boolean;
  source?: string;
  projectIds?: string[];
}

export interface AgentStage {
  id: string;
  name: string;
  status: 'pending' | 'running' | 'success' | 'failed' | 'skipped';
  icon: string;
  steps: PipelineStep[];
  duration?: number;
}

export interface PipelineStep {
  id: string;
  name: string;
  status: 'pending' | 'running' | 'success' | 'failed' | 'skipped';
  duration?: number;
  output?: string;
}

export interface TokenUsage {
  inputTokens: number;
  outputTokens: number;
  estimatedCostUsd?: number;
  model?: string;
}

export interface PlanInfo {
  title: string;
  description: string;
  strategy: string[];
  riskWarnings: string[];
  fileChanges: FileChange[];
  targetProject: string;
  estimatedTokens?: number;
  estimatedCost?: number;
  thinkingTime?: number;
  impactLevel?: 'low' | 'medium' | 'high';
}

export interface FileChange {
  path: string;
  type: 'create' | 'modify' | 'delete';
  additions: number;
  deletions: number;
  preview?: string;
}
