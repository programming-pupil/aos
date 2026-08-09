export function isNl2sqlAttributionTaskId(
  taskId: string | null | undefined,
): boolean {
  return (taskId ?? "").startsWith("nl2sql-attribution-task-");
}

export function isChatAdversarialRunId(
  taskId: string | null | undefined,
): boolean {
  return (taskId ?? "").startsWith("chat-adv-");
}

export function isPmResearchTaskId(
  taskId: string | null | undefined,
): taskId is string {
  const normalized = taskId?.trim() ?? "";
  return normalized.startsWith("pm-research-task-");
}
