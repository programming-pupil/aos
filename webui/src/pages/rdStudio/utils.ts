import dayjs from 'dayjs';
import type {
  ApiKeyRecord,
  RdRepository,
  RdRepositoryFileSuggestion,
  RdTask,
  RdTaskMode,
} from '@/types';
import type { RdTaskThreadSummary } from './types';
import {
  RD_EXPLAIN_INTENT_HINTS,
  RD_MODIFY_INTENT_HINTS,
  RD_REVIEW_INTENT_HINTS,
} from './constants';

function hasIntentHint(text: string, hints: string[]) {
  return hints.some((hint) => text.includes(hint));
}

export function inferRdTaskMode(prompt: string): RdTaskMode {
  const text = prompt.trim().toLowerCase();
  if (!text) return 'ask';
  const wantsModify = hasIntentHint(text, RD_MODIFY_INTENT_HINTS);
  if (wantsModify) return 'modify';
  if (hasIntentHint(text, RD_REVIEW_INTENT_HINTS)) return 'review';
  if (hasIntentHint(text, RD_EXPLAIN_INTENT_HINTS)) return 'explain';
  return 'ask';
}

export function asRdTaskMode(value: string | undefined | null, fallback: RdTaskMode = 'ask'): RdTaskMode {
  return value === 'ask' || value === 'modify' || value === 'explain' || value === 'review'
    ? value
    : fallback;
}

export function isRdModel(key: ApiKeyRecord): boolean {
  if (!key.enabled || key.model_type !== 'chat') return false;
  if (key.runtime_available === false) return false;
  const scenarios = key.scenarios;
  return !scenarios || scenarios.length === 0 || scenarios.includes('rd') || scenarios.includes('agent');
}

export function modelLabel(key: ApiKeyRecord): string {
  const model = key.model || key.name;
  return `${model} · ${key.provider}`;
}

export function repoLabel(repo?: RdRepository | null): string {
  if (!repo) return '';
  return `${repo.name} · ${repo.branch}`;
}

export function safeMentionRepoName(repo: RdRepository): string {
  const compact = repo.name.trim().replace(/\s+/g, '_').replace(/[:@]/g, '_');
  return compact || repo.id.slice(0, 8);
}

export function buildFileMentionValue(
  repo: RdRepository,
  file: RdRepositoryFileSuggestion,
  primaryRepoId?: string,
): string {
  if (repo.id === primaryRepoId) {
    return file.path;
  }
  return `${safeMentionRepoName(repo)}:${file.path}`;
}

export function rdTaskThreadId(task: RdTask): string {
  return task.threadId?.trim() || task.id;
}

export function rdTaskThreadTitle(task: RdTask): string {
  return task.threadTitle?.trim() || task.title;
}

export function rdTaskIteration(task: RdTask): number {
  return task.iterationNo && task.iterationNo > 0 ? task.iterationNo : 1;
}

export function compareRdTasksDesc(a: RdTask, b: RdTask): number {
  const iterationDiff = rdTaskIteration(b) - rdTaskIteration(a);
  if (iterationDiff !== 0) return iterationDiff;
  return dayjs(b.createdAt).valueOf() - dayjs(a.createdAt).valueOf();
}

export function buildRdTaskThreadSummaries(tasks: RdTask[]): RdTaskThreadSummary[] {
  const map = new Map<string, RdTaskThreadSummary>();
  for (const task of tasks) {
    const threadId = rdTaskThreadId(task);
    const existing = map.get(threadId);
    if (!existing) {
      map.set(threadId, {
        threadId,
        title: rdTaskThreadTitle(task),
        latest: task,
        tasks: [task],
        count: 1,
      });
      continue;
    }
    existing.tasks.push(task);
    existing.count = existing.tasks.length;
    existing.tasks.sort(compareRdTasksDesc);
    existing.latest = existing.tasks[0];
    existing.title = rdTaskThreadTitle(existing.latest);
  }
  return Array.from(map.values()).sort(
    (a, b) => dayjs(b.latest.createdAt).valueOf() - dayjs(a.latest.createdAt).valueOf(),
  );
}

function runtimeItemLabel(value: unknown): string | null {
  if (typeof value === 'string') return value;
  if (!value || typeof value !== 'object') return null;
  const item = value as Record<string, unknown>;
  for (const key of ['name', 'serverName', 'server', 'skillName', 'id', 'title']) {
    const candidate = item[key];
    if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
  }
  return null;
}

export function asRuntimeStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  const result: string[] = [];
  for (const item of value) {
    const label = runtimeItemLabel(item);
    if (label && !seen.has(label)) {
      seen.add(label);
      result.push(label);
    }
  }
  return result;
}

export function asRuntimeRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

export function runtimeNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

export function runtimeStringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
    : [];
}

export function runtimeRecordArray(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter((item): item is Record<string, unknown> => !!item && typeof item === 'object' && !Array.isArray(item))
    : [];
}

export function runtimeNumberArray(value: unknown): number[] {
  return Array.isArray(value)
    ? value.filter((item): item is number => typeof item === 'number' && Number.isFinite(item))
    : [];
}
