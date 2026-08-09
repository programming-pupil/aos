import type { ChatAdversarialRun } from '@/types';

export type TraceAnswer = {
  model?: string;
  answer?: string;
  error?: string | null;
  durationMs?: number;
  consensusVote?: {
    acceptConsensus?: boolean;
    preferredWinnerModel?: string | null;
    remainingObjections?: string[];
    evidenceQueries?: string[];
  } | null;
  evidenceRequest?: {
    needed?: boolean;
    queries?: string[];
    reason?: string | null;
  } | null;
};

export type TraceRound = {
  round?: number;
  phase?: string;
  answers?: TraceAnswer[];
  judge?: {
    resolved?: boolean;
    winnerModel?: string | null;
    winnerReason?: string | null;
    raw?: string | null;
  };
  participantConsensus?: {
    reached?: boolean;
    acceptedModels?: string[];
    missingOrRejectedModels?: string[];
    remainingObjections?: string[];
    preferredWinnerModels?: string[];
  };
  evidenceSearch?: {
    query?: string;
    afterRound?: number;
    available?: boolean;
    resultCount?: number;
    degradedReason?: string | null;
    searchNumber?: number;
  };
};

export type TraceShape = {
  rounds?: TraceRound[];
  final?: {
    winnerModel?: string | null;
    winnerReason?: string | null;
    finalAnswer?: string | null;
  };
};

export type TimelineMessage = {
  id: string;
  role: 'user' | 'model' | 'judge' | 'final' | 'system';
  title: string;
  subtitle?: string;
  content: string;
  model?: string;
  round?: number;
  error?: boolean;
  typing?: boolean;
  animate?: boolean;
};

export type ThreadSummary = {
  threadId: string;
  latest: ChatAdversarialRun;
  count: number;
};

export type AntdFormValidationError = {
  errorFields?: Array<{
    name?: Array<string | number>;
    errors?: string[];
  }>;
};
