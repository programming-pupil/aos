import { describe, expect, it } from 'vitest';
import type { SqlKnowledgeImportTask } from '@/types';
import { selectActiveSqlKnowledgeImportTask } from './sqlKnowledgeImportTasks';

function task(
  id: string,
  status: SqlKnowledgeImportTask['status'],
  processedFiles: number,
): SqlKnowledgeImportTask {
  return {
    id,
    packId: 'pack-1',
    datasourceId: 'ds-1',
    status,
    totalFiles: 45,
    processedFiles,
    failedFiles: 0,
    currentFilename: status === 'running' ? 'query.sql' : null,
    errorMessage: null,
    failureDetails: [],
    createdAt: '2026-08-11T00:00:00Z',
    startedAt: null,
    completedAt: null,
    updatedAt: '2026-08-11T00:00:00Z',
  };
}

describe('selectActiveSqlKnowledgeImportTask', () => {
  it('shows the running task before a newer queued task', () => {
    const selected = selectActiveSqlKnowledgeImportTask([
      task('new-pending', 'pending', 0),
      task('active', 'running', 19),
    ]);

    expect(selected?.id).toBe('active');
    expect(selected?.processedFiles).toBe(19);
  });

  it('keeps showing the running batch at its latest progress instead of a queued duplicate', () => {
    const selected = selectActiveSqlKnowledgeImportTask([
      task('duplicate-pending', 'pending', 0),
      task('active', 'running', 45),
    ]);

    expect(selected?.id).toBe('active');
    expect(selected?.processedFiles).toBe(45);
  });

  it('does not regress from a completed batch to an older orphaned pending row', () => {
    const completed = task('completed', 'completed', 45);
    completed.createdAt = '2026-08-11T00:00:00Z';
    completed.updatedAt = '2026-08-11T00:10:00Z';
    completed.completedAt = '2026-08-11T00:10:00Z';
    const orphaned = task('orphaned', 'pending', 0);
    orphaned.createdAt = '2026-08-11T00:01:00Z';

    expect(selectActiveSqlKnowledgeImportTask([orphaned, completed])).toBeUndefined();
  });
});
