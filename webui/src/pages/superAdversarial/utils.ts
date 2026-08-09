import type { TFunction } from 'i18next';
import type { ChatAdversarialRun } from '@/types';
import type { AntdFormValidationError, ThreadSummary, TimelineMessage, TraceShape } from './types';

export const ADVERSARIAL_DEFAULT_MAX_ROUNDS = 3;
export const ADVERSARIAL_HARD_MAX_ROUNDS = 8;

export function isAntdFormValidationError(error: unknown): error is AntdFormValidationError {
  return Boolean(
    error &&
      typeof error === 'object' &&
      Array.isArray((error as AntdFormValidationError).errorFields)
  );
}

export function scenarioAppliesToChat(scenarios?: string[] | null): boolean {
  return !scenarios || scenarios.length === 0 || scenarios.includes('chat');
}

export function isChatModelKey(key: {
  enabled?: boolean;
  model_type?: string;
  model?: string | null;
  scenarios?: string[] | null;
}): boolean {
  return Boolean(
    key.enabled &&
      key.model_type === 'chat' &&
      key.model?.trim() &&
      scenarioAppliesToChat(key.scenarios)
  );
}

export function parseTrace(trace?: Record<string, unknown> | null): TraceShape {
  if (!trace || typeof trace !== 'object') return {};
  return trace as TraceShape;
}

export function getRunThreadId(run: ChatAdversarialRun): string {
  return run.thread_id?.trim() || run.id;
}

export function getThreadDisplayTitle(run: ChatAdversarialRun): string {
  return run.thread_title?.trim() || run.question;
}

function adversarialMessageModelPart(model?: string | null): string {
  return (model?.trim() || 'system')
    .split('')
    .map((ch) => (/^[a-z0-9]$/i.test(ch) ? ch : '-'))
    .join('');
}

function adversarialModelMessageId(
  run: ChatAdversarialRun,
  phase: string,
  round: number | undefined,
  model?: string | null,
): string {
  return `${run.id}-${phase}-${round && round > 0 ? round : 'final'}-${adversarialMessageModelPart(model)}`;
}

export function buildRunTimeline(run: ChatAdversarialRun, t: TFunction): TimelineMessage[] {
  const trace = parseTrace(run.trace);
  const messages: TimelineMessage[] = [
    {
      id: `${run.id}-user`,
      role: 'user',
      title: t('chat.adversarialUser'),
      subtitle: t('chat.adversarialIterationWithTime', {
        iteration: run.iteration_no || 1,
        time: run.created_at,
      }),
      content: run.question,
    },
  ];

  for (const round of trace.rounds ?? []) {
    const roundNo = round.round ?? 0;
    for (const [idx, answer] of (round.answers ?? []).entries()) {
      const model = answer.model || t('chat.adversarialUnknownModel');
      messages.push({
        id: adversarialModelMessageId(
          run,
          round.phase === 'review' ? 'review' : 'initial',
          roundNo,
          answer.model || model || `model-${idx}`,
        ),
        role: 'model',
        title: model,
        subtitle: t('chat.adversarialRoundSpeech', { round: roundNo || '?' }),
        content: answer.error || answer.answer || t('chat.adversarialNoTrace'),
        model,
        round: roundNo,
        error: Boolean(answer.error),
      });
    }
    if (round.judge) {
      const judgeModel = run.judge_model || t('chat.adversarialUnknownModel');
      messages.push({
        id: adversarialModelMessageId(run, 'judge', roundNo, run.judge_model),
        role: 'judge',
        title: t('chat.adversarialJudgeWithModel', { model: judgeModel }),
        subtitle: t('chat.adversarialRoundJudge', { round: roundNo || '?' }),
        content:
          round.judge.winnerReason ||
          round.judge.raw ||
          (round.judge.resolved ? t('chat.adversarialResolved') : t('chat.adversarialNotResolved')),
        model: judgeModel,
      });
    }
  }

  if (run.final_answer) {
    const judgeModel = run.judge_model || t('chat.adversarialUnknownModel');
    messages.push({
      id: adversarialModelMessageId(run, 'final', undefined, run.judge_model),
      role: 'final',
      title: t('chat.adversarialFinalWithModel', { model: judgeModel }),
      subtitle: run.winner_model
        ? t('chat.adversarialWinnerWithModel', { model: run.winner_model })
        : undefined,
      content: run.final_answer,
      model: judgeModel,
    });
  } else if (run.status === 'queued' || run.status === 'running' || run.status === 'cancelling') {
    messages.push({
      id: `${run.id}-system-running`,
      role: 'system',
      title: t('chat.adversarialSystem'),
      content:
        run.status === 'cancelling'
          ? t('chat.adversarialCancellingHint')
          : t('chat.adversarialRunningHint'),
    });
  }

  return messages;
}

export function buildThreadTimeline(runs: ChatAdversarialRun[], t: TFunction): TimelineMessage[] {
  return runs.flatMap((run) => buildRunTimeline(run, t));
}

export function messageAccent(role: TimelineMessage['role'], model?: string): string {
  if (role === 'user') return '#2563eb';
  if (role === 'judge') return '#a16207';
  if (role === 'final') return '#047857';
  if (role === 'system') return '#64748b';
  const palette = ['#0f766e', '#b91c1c', '#6d28d9', '#0369a1', '#a16207'];
  let hash = 0;
  for (const ch of model ?? 'model') hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  return palette[hash % palette.length];
}

export function adversarialStatusColor(status: string): string {
  switch (status) {
    case 'completed':
      return 'green';
    case 'failed':
      return 'red';
    case 'running':
      return 'processing';
    case 'cancelling':
      return 'orange';
    case 'cancelled':
      return 'default';
    default:
      return 'default';
  }
}

export function summarizeThreads(runs: ChatAdversarialRun[]): ThreadSummary[] {
  const map = new Map<string, ThreadSummary>();
  for (const run of runs) {
    const threadId = getRunThreadId(run);
    const existing = map.get(threadId);
    if (!existing) {
      map.set(threadId, { threadId, latest: run, count: run.iteration_no || 1 });
      continue;
    }
    existing.count = Math.max(existing.count, run.iteration_no || 1);
    if (
      (run.iteration_no ?? 1) > (existing.latest.iteration_no ?? 1) ||
      ((run.iteration_no ?? 1) === (existing.latest.iteration_no ?? 1) &&
        run.created_at > existing.latest.created_at)
    ) {
      existing.latest = run;
    }
  }
  return Array.from(map.values()).sort((a, b) => {
    const pinnedDiff = Number(Boolean(b.latest.thread_pinned)) - Number(Boolean(a.latest.thread_pinned));
    if (pinnedDiff !== 0) return pinnedDiff;
    const bTime = b.latest.updated_at || b.latest.created_at;
    const aTime = a.latest.updated_at || a.latest.created_at;
    return bTime.localeCompare(aTime);
  });
}

export function activeRunStatuses(status?: string | null): boolean {
  return status === 'queued' || status === 'running' || status === 'cancelling';
}
