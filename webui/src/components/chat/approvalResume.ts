import type {
  AgentSessionStreamHandlers,
  RuntimeApprovalPaused,
  RuntimeQuestionPaused,
} from "@/api/agent";

export interface ApprovalResumeCallbacks {
  onApprovalRequired: (paused: RuntimeApprovalPaused) => void;
  onQuestionRequired?: (paused: RuntimeQuestionPaused) => void;
  onStreamEnd: () => void;
  onError: (error: string) => void;
}

export interface SessionStreamHandlers {
  sessionId: string;
  handlers: AgentSessionStreamHandlers;
}

/**
 * Keep a live stream's handlers intact, but provide a reload-safe minimal
 * handler set when a durable approval is resumed from persisted state.
 */
export function approvalResumeHandlers(
  sessionId: string,
  existing: SessionStreamHandlers | null | undefined,
  callbacks: ApprovalResumeCallbacks,
): AgentSessionStreamHandlers {
  if (existing?.sessionId === sessionId) return existing.handlers;
  return {
    onApprovalRequired: callbacks.onApprovalRequired,
    onQuestionRequired: callbacks.onQuestionRequired,
    onStreamEnd: callbacks.onStreamEnd,
    onError: callbacks.onError,
  };
}
