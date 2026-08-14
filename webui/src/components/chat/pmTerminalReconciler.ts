import type { PmFinalDeliveryArtifact, PmReportArtifact } from "./chatCore.pmTypes";
import type { DisplayMessage } from "./chatCore.types";
import { contentToPlain } from "./chatCore.utils";

function pmTerminalTextIsFailure(text: string): boolean {
  return (
    text.startsWith("研究任务失败：") ||
    text.startsWith("研究任务已取消。") ||
    text.startsWith("研究任务已完成，但未返回")
  );
}

export function compactPmDuplicateTaskReplies(messages: DisplayMessage[]): DisplayMessage[] {
  const out: DisplayMessage[] = [];
  const firstReplyByTaskId = new Map<string, number>();
  let changed = false;

  for (const msg of messages) {
    if (msg.role === "assistant" && msg.pmTaskId) {
      const existingIdx = firstReplyByTaskId.get(msg.pmTaskId);
      if (existingIdx != null) {
        const existing = out[existingIdx];
        const existingPlain = contentToPlain(existing.content).trim();
        const incomingPlain = contentToPlain(msg.content).trim();
        const incomingIsRicher =
          (!incomingPlain.startsWith("研究任务失败：") &&
            (existingPlain.startsWith("研究任务失败：") ||
              incomingPlain.length > existingPlain.length)) ||
          (!!msg.pmReport && !existing.pmReport) ||
          (!!msg.pmFinalDelivery && !existing.pmFinalDelivery);
        if (incomingIsRicher) {
          out[existingIdx] = {
            ...existing,
            content: msg.content,
            toolCalls: msg.toolCalls ?? existing.toolCalls,
            evidenceSources: msg.evidenceSources ?? existing.evidenceSources,
            thinking: msg.thinking ?? existing.thinking,
            thinkingDurationMs:
              msg.thinkingDurationMs ?? existing.thinkingDurationMs,
            pmTaskStatus: msg.pmTaskStatus ?? existing.pmTaskStatus,
            pmReport: msg.pmReport ?? existing.pmReport,
            pmFinalDelivery: msg.pmFinalDelivery ?? existing.pmFinalDelivery,
            pmSearchUsage: msg.pmSearchUsage ?? existing.pmSearchUsage,
            traceEvents: msg.traceEvents ?? existing.traceEvents,
          };
        }
        changed = true;
        continue;
      }
      firstReplyByTaskId.set(msg.pmTaskId, out.length);
    }
    out.push(msg);
  }

  return changed ? out : messages;
}

export function reconcilePmHistoryTerminalAssistant(
  messages: DisplayMessage[],
  terminalText: string,
  taskMeta?: {
    taskId?: string | null;
    taskStatus?: string | null;
    pmReport?: PmReportArtifact;
    pmFinalDelivery?: PmFinalDeliveryArtifact;
    userMessageId?: string | null;
    preserveRicherContent?: boolean;
  },
): DisplayMessage[] {
  const normalized = terminalText.trim();
  if (!normalized) return messages;

  const next = [...messages];
  const taskId = taskMeta?.taskId?.trim() || null;
  const taskStatus = taskMeta?.taskStatus ?? undefined;
  const buildReply = (existing?: DisplayMessage): DisplayMessage => ({
    id: existing?.id ?? `hist-pm-terminal-${taskId ?? "local"}-${Date.now()}`,
    role: "assistant" as const,
    content: normalized,
    timestamp: existing?.timestamp ?? Date.now(),
    createdAt: existing?.createdAt,
    toolCalls: existing?.toolCalls,
    evidenceSources: existing?.evidenceSources,
    thinking: existing?.thinking,
    thinkingDurationMs: existing?.thinkingDurationMs,
    pmTaskId: taskId ?? existing?.pmTaskId,
    pmTaskStatus: taskStatus ?? existing?.pmTaskStatus,
    pmReport: taskMeta?.pmReport ?? existing?.pmReport,
    pmFinalDelivery: taskMeta?.pmFinalDelivery ?? existing?.pmFinalDelivery,
    pmSearchUsage: existing?.pmSearchUsage,
    traceEvents: existing?.traceEvents,
  });
  const writeReplyAt = (idx: number): DisplayMessage[] => {
    const current = next[idx];
    const currentPlain = contentToPlain(current.content).trim();
    if (currentPlain === normalized) {
      const hasFreshMeta =
        (!!taskId && current.pmTaskId !== taskId) ||
        (!!taskStatus && current.pmTaskStatus !== taskStatus) ||
        (!!taskMeta?.pmReport && current.pmReport !== taskMeta.pmReport) ||
        (!!taskMeta?.pmFinalDelivery &&
          current.pmFinalDelivery !== taskMeta.pmFinalDelivery);
      if (!hasFreshMeta) return compactPmDuplicateTaskReplies(next);
    }
    if (
      taskMeta?.preserveRicherContent === true &&
      !pmTerminalTextIsFailure(normalized) &&
      currentPlain.length > normalized.length
    ) {
      next[idx] = {
        ...current,
        pmTaskId: taskId ?? current.pmTaskId,
        pmTaskStatus: taskStatus ?? current.pmTaskStatus,
        pmReport: taskMeta.pmReport ?? current.pmReport,
        pmFinalDelivery:
          taskMeta.pmFinalDelivery ?? current.pmFinalDelivery,
      };
      return compactPmDuplicateTaskReplies(next);
    }
    if (
      pmTerminalTextIsFailure(normalized) &&
      !currentPlain.startsWith("研究任务失败：") &&
      currentPlain.length > 0
    ) {
      return messages;
    }
    next[idx] = {
      ...current,
      ...buildReply(current),
    };
    return compactPmDuplicateTaskReplies(next);
  };

  if (taskId) {
    const existingTaskReplyIdx = next.findIndex(
      (msg) => msg.role === "assistant" && msg.pmTaskId === taskId,
    );
    if (existingTaskReplyIdx >= 0) {
      return writeReplyAt(existingTaskReplyIdx);
    }
  }

  const targetUserMessageId = taskMeta?.userMessageId ?? null;
  if (targetUserMessageId) {
    const userIdx = next.findIndex(
      (msg) => msg.id === targetUserMessageId && msg.role === "user",
    );
    if (userIdx >= 0) {
      const nextUserIdx = next.findIndex(
        (msg, idx) => idx > userIdx && msg.role === "user",
      );
      const replySearchEnd = nextUserIdx >= 0 ? nextUserIdx : next.length;
      const existingReplyIdx = next.findIndex(
        (msg, idx) =>
          idx > userIdx &&
          idx < replySearchEnd &&
          msg.role === "assistant" &&
          (!taskId || !msg.pmTaskId || msg.pmTaskId === taskId),
      );
      if (existingReplyIdx >= 0) {
        return writeReplyAt(existingReplyIdx);
      }
      next.splice(userIdx + 1, 0, buildReply());
      return compactPmDuplicateTaskReplies(next);
    }
  }

  if (taskId) {
    const sameTextReplyIdx = next.findIndex(
      (msg) =>
        msg.role === "assistant" &&
        !msg.pmTaskId &&
        contentToPlain(msg.content).trim() === normalized,
    );
    if (sameTextReplyIdx >= 0) {
      return writeReplyAt(sameTextReplyIdx);
    }
  }

  let lastAssistantIdx = -1;
  let lastUserIdx = -1;
  for (let i = next.length - 1; i >= 0; i -= 1) {
    if (next[i].role === "user" && lastUserIdx < 0) {
      lastUserIdx = i;
    }
    if (next[i].role === "assistant") {
      lastAssistantIdx = i;
      break;
    }
  }
  if (lastAssistantIdx < 0) {
    next.push(buildReply());
    return compactPmDuplicateTaskReplies(next);
  }

  if (lastUserIdx > lastAssistantIdx) {
    next.push(buildReply());
    return compactPmDuplicateTaskReplies(next);
  }

  const current = next[lastAssistantIdx];
  if (taskId && current.pmTaskId && current.pmTaskId !== taskId) {
    return messages;
  }
  const currentPlain = contentToPlain(current.content).trim();
  if (currentPlain === normalized) return messages;
  if (
    pmTerminalTextIsFailure(normalized) &&
    !currentPlain.startsWith("研究任务失败：") &&
    currentPlain.length > 0
  ) {
    return messages;
  }
  if (
    taskMeta?.preserveRicherContent === true &&
    currentPlain.length > normalized.length
  ) {
    next[lastAssistantIdx] = {
      ...current,
      pmTaskId: taskId ?? current.pmTaskId,
      pmTaskStatus: taskStatus ?? current.pmTaskStatus,
      pmReport: taskMeta.pmReport ?? current.pmReport,
    };
    return compactPmDuplicateTaskReplies(next);
  }
  next[lastAssistantIdx] = {
    ...current,
    content: normalized,
    timestamp: current.timestamp ?? Date.now(),
    pmTaskId: taskId ?? current.pmTaskId,
    pmTaskStatus: taskStatus ?? current.pmTaskStatus,
    pmReport: taskMeta?.pmReport ?? current.pmReport,
    pmSearchUsage: current.pmSearchUsage,
    traceEvents: current.traceEvents,
  };
  return compactPmDuplicateTaskReplies(next);
}
