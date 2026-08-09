import type {
  AppliedRuleHit,
  ConversationMessage,
  QueryUnderstandingResponse,
  Nl2sqlReferenceUsage,
} from '@/types';

export interface QueryResult {
  columns: string[];
  rows: Record<string, unknown>[];
  row_count: number;
  total_rows?: number;
  has_more?: boolean;
  limit?: number;
  offset?: number;
  execution_time_ms?: number;
}

export interface QueryStageTimelineItem {
  stage: string;
  message?: string | null;
  atElapsedMs: number;
  stageElapsedMs?: number | null;
  kind?: 'stage' | 'step';
  label?: string;
}

export interface AgentStepResult {
  step_id: number;
  step_type: string;
  datasource_id: string | null;
  description: string;
  sql?: string | null;
  columns: string[];
  rows: Record<string, unknown>[];
  row_count: number;
  execution_ms: number;
  error: string | null;
}

export interface NlTurn {
  id: string;
  role: 'user' | 'assistant' | 'system';
  question?: string;
  sql?: string;
  explanation?: string;
  result?: QueryResult;
  resultSource?: 'query' | 'agent';
  error?: string | null;
  dataSourceId?: string;
  isGenerating?: boolean;
  isExecuting?: boolean;
  editedSql?: string;
  queryId?: string;
  feedback?: 'up' | 'down' | null;
  queryUnderstanding?: QueryUnderstandingResponse;
  quError?: string;
  clarificationQuestion?: string | null;
  clarificationFallbackMode?: string | null;
  clarificationTurn?: number | null;
  clarificationConfirmedRequirements?: string[] | null;
  clarificationMissingRequirements?: string[] | null;
  appliedRules?: AppliedRuleHit[];
  usedReferences?: Nl2sqlReferenceUsage[];
  queryTaskId?: string | null;
  queryStage?: string | null;
  queryStageMessage?: string | null;
  queryElapsedMs?: number | null;
  queryStageHistory?: string[];
  queryStageTimeline?: QueryStageTimelineItem[];
  clarifyTaskId?: string | null;
  clarifyStage?: string | null;
  clarifyStageMessage?: string | null;
  resultScore?: number | null;
  validationWarnings?: import('@/types').ValidationWarning[] | null;
  validationSuggestions?: string[] | null;
  autoExecuteFailed?: boolean;
  routeConfidence?: number | null;
  routingMethod?: string | null;
  semanticContext?: Record<string, unknown> | unknown[] | null;
  multiSourceSteps?: AgentStepResult[];
  multiSourceTotalExecutionMs?: number | null;
}

export interface SchemaTable {
  name: string;
  description?: string;
  columns: SchemaColumn[];
}

export interface SchemaColumn {
  name: string;
  type: string;
  description?: string;
  nullable?: boolean;
  primary_key?: boolean;
}

export type ChartType = 'line' | 'bar' | 'pie' | 'scatter' | 'heatmap';

export type ViewTab = 'table' | 'chart' | 'explain';

export interface EditableView {
  id: string;
  query_id: string;
  conversation_id?: string | null;
  name: string;
  question: string;
  sql: string;
  data_source_id: string | null;
  description?: string | null;
  created_at: string;
}

export type ConversationTurnMapper = (messages: ConversationMessage[]) => NlTurn[];
