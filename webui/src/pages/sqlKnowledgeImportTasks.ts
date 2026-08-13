import type { SqlKnowledgeImportTask } from '@/types';

export function selectActiveSqlKnowledgeImportTask(
  tasks: SqlKnowledgeImportTask[],
): SqlKnowledgeImportTask | undefined {
  if (tasks.length === 0) return undefined;

  // Import history can contain a legacy pending row left behind by an older
  // client that submitted one directory once per file. Treat the newest task
  // as authoritative: once it reaches a terminal state, an older 0/N pending
  // row must not make the UI jump back from  N/N to 0/N.
  const byCreatedAtDesc = [...tasks].sort((a, b) => {
    const aTime = Date.parse(a.createdAt || '') || 0;
    const bTime = Date.parse(b.createdAt || '') || 0;
    return bTime - aTime;
  });
  const latestTerminal = byCreatedAtDesc.find((task) =>
    !['pending', 'running'].includes(task.status),
  );
  const terminalTime = latestTerminal
    ? Date.parse(latestTerminal.completedAt || latestTerminal.updatedAt || latestTerminal.createdAt || '') || 0
    : 0;
  const active = byCreatedAtDesc.filter((task) => {
    if (!['pending', 'running'].includes(task.status)) return false;
    // Older clients could leave a 0/N pending row after the real batch had
    // already completed. Do not make the progress bar regress unless this is
    // a genuinely newer import submitted after that completion.
    const activityTime = Math.max(
      Date.parse(task.createdAt || '') || 0,
      Date.parse(task.startedAt || '') || 0,
      Date.parse(task.updatedAt || '') || 0,
    );
    return !terminalTime || activityTime > terminalTime;
  });
  return active.find((task) => task.status === 'running')
    ?? active.find((task) => task.status === 'pending');
}
