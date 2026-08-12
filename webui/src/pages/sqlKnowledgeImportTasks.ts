import type { SqlKnowledgeImportTask } from '@/types';

export function selectActiveSqlKnowledgeImportTask(
  tasks: SqlKnowledgeImportTask[],
): SqlKnowledgeImportTask | undefined {
  return tasks.find((task) => task.status === 'running')
    ?? tasks.find((task) => task.status === 'pending');
}
