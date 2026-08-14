import type { ContentBlock } from "@/types";
import type { PmTaskDocumentInput, PmTaskImageInput } from "@/api";

export type PmQueuedPrompt = {
  id: string;
  userMessageId: string;
  content: string | ContentBlock[];
  finalMessage: string;
  images: PmTaskImageInput[];
  documents: PmTaskDocumentInput[];
  createdAt: number;
  source: "input" | "quick_fix" | "replace";
  appendUserMessage?: boolean;
  replyTo?: string;
};

export type PmQueuedPromptDraft = Omit<
  PmQueuedPrompt,
  "id" | "createdAt" | "userMessageId"
> & {
  userMessageId?: string;
};

export type PmStageId =
  | "preflight"
  | "understand"
  | "report_extract"
  | "task_plan"
  | "planner"
  | "retrieve"
  | "deep_loop"
  | "verify"
  | "retry_repair"
  | "synthesize"
  | "turn_model_started"
  | "native_web_search"
  | "runtime_wait"
  | "verification_repair";

export type PmStageStatus =
  | "pending"
  | "running"
  | "completed"
  | "degraded"
  | "skipped"
  | "failed";

export interface PmStageState {
  stage: string;
  status: PmStageStatus;
  attempt: number;
  detail?: Record<string, unknown>;
  runningSince?: number;
  updatedAt: number;
}

export interface PmStageEvent {
  stage: string;
  status: PmStageStatus;
  attempt: number;
  detail?: Record<string, unknown>;
  at: number;
}

export interface PmClaimEvidence {
  claim: string;
  evidence_excerpt?: string;
  urls: string[];
  cited: boolean;
}

export interface PmConflictRow {
  topic: string;
  source_a: string;
  claim_a: string;
  source_b: string;
  claim_b: string;
  verdict: string;
}

export interface PmEvidenceLeaf {
  url: string;
  domain: string;
  excerpt: string;
}

export interface PmEvidenceTreeNode {
  claim: string;
  status: string;
  evidence_count: number;
  evidences: PmEvidenceLeaf[];
}

export interface PmConflictGraphEdge {
  topic: string;
  source_left: string;
  source_right: string;
  relation: string;
  verdict: string;
  confidence: number;
  urls: string[];
}

export interface PmConflictGraph {
  topic_count: number;
  edge_count: number;
  adjudicated_count: number;
  unresolved_count: number;
  avg_confidence: number;
  edges: PmConflictGraphEdge[];
}

export interface PmReportArtifact {
  schemaVersion?: string;
  questionType?: string;
  quantEnabled?: boolean;
  reportJson?: Record<string, unknown>;
  reportHtml?: string;
  reportJsonV3?: Record<string, unknown>;
  reportHtmlV3?: string;
}

/** Durable projection written by the PM task finalizer.  It is deliberately
 * separate from the chat text so a history reload cannot lose the delivery
 * card when the latest assistant message is reconstructed. */
export interface PmFinalDeliveryArtifact {
  schemaVersion: string;
  taskId: string;
  taskStatus: string;
  qualityStatus: string;
  deliveryStatus: string;
  response?: {
    text?: string;
    pm_quality?: Record<string, unknown>;
    pm_report?: PmReportArtifact;
    [key: string]: unknown;
  } | null;
  stages: Array<{
    stage: string;
    status: string;
    attempt: number;
    detail?: Record<string, unknown> | null;
    lastEventSeq: number;
    updatedAt: string;
  }>;
  contentHash: string;
}

export interface PmToolSummarySample {
  idx: number;
  tool: string;
  source?: string;
  isError?: boolean;
  durationMs?: number;
  input?: string;
  output?: string;
}

export interface PmToolSummary {
  count: number;
  errorCount: number;
  byName: Array<{ name: string; count: number; errorCount: number }>;
  samples: PmToolSummarySample[];
}

export interface PmSearchLayerUsageRow {
  layer: string;
  label?: string;
  attempts: number;
  successCount: number;
  errorCount: number;
  skippedCount?: number;
  resultCount?: number;
}

export interface PmSearchUsageSummary {
  rows: PmSearchLayerUsageRow[];
}

export interface PmLiveToolEvent {
  phase: "start" | "result" | "error";
  index: number;
  tool: string;
  source?: string;
  target?: string;
  durationMs?: number;
  isError?: boolean;
}

export interface PmQualitySnapshot {
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
  claim_alignment?: PmClaimEvidence[];
  evidence_tree?: PmEvidenceTreeNode[];
  conflict_matrix?: PmConflictRow[];
  conflict_graph?: PmConflictGraph;
  missing: string[];
  suggestions: string[];
}

export interface PmInlineAction {
  id: string;
  index: number;
  name: string;
  source: "mcp" | "builtin" | "skill";
  sourceLabel?: string;
  status: "pending" | "running" | "success" | "error";
  durationMs?: number;
  detail?: string;
  createdAt: number;
  updatedAt: number;
}

export interface PmInlineSegment {
  id: string;
  stage: string;
  attempt: number;
  status: PmStageStatus;
  summary: string;
  rawDetail?: Record<string, unknown>;
  excerpt: string;
  actions: PmInlineAction[];
  createdAt: number;
  updatedAt: number;
}

export interface PmStrategyRunRecord {
  key: string;
  at: number;
  sessionId?: string;
  route: string;
  channel?: string;
  variant?: string;
  passed: boolean;
  citationCount: number;
  domainCount: number;
  toolCallCount: number;
  retrieveDurationMs?: number;
}

export interface PmStrategyLeaderboardRow {
  routeKey: string;
  route: string;
  channel?: string;
  runs: number;
  passRate: number;
  avgCitationCount: number;
  avgDomainCount: number;
  avgRetrieveDurationMs: number | null;
  score: number;
  latestAt: number;
}

export function shouldShowPmPostStreamNotice(
  streamCommitted: boolean,
  backgroundTaskRunning: boolean,
): boolean {
  return !streamCommitted || backgroundTaskRunning;
}

export const PM_STAGE_ORDER: PmStageId[] = [
  "preflight",
  "understand",
  "report_extract",
  "task_plan",
  "planner",
  "retrieve",
  "deep_loop",
  "verify",
  "retry_repair",
  "synthesize",
];

export const PM_PIPELINE_BUDGET_MS = 360 * 1000;

export const PM_STAGE_BUDGET_MS: Partial<Record<PmStageId, number>> = {
  preflight: 10 * 1000,
  understand: 18 * 1000,
  task_plan: 18 * 1000,
  planner: 12 * 1000,
  retrieve: 220 * 1000,
  deep_loop: 80 * 1000,
  verify: 10 * 1000,
  retry_repair: 70 * 1000,
  synthesize: 22 * 1000,
};
