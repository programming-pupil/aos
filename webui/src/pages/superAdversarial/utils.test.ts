import type { TFunction } from 'i18next';
import { describe, expect, it } from 'vitest';
import type { ChatAdversarialRun } from '@/types';
import { buildRunTimeline } from './utils';

const t = ((key: string, options?: Record<string, unknown>) => {
  const model = String(options?.model ?? 'unknown');
  const round = String(options?.round ?? '?');
  const values: Record<string, string> = {
    'chat.adversarialUser': 'User',
    'chat.adversarialUnknownModel': 'Unknown model',
    'chat.adversarialJudgeWithModel': `Judge model: ${model}`,
    'chat.adversarialFinalWithModel': `Final answer · Synthesis model: ${model}`,
    'chat.adversarialWinnerWithModel': `Winner: ${model}`,
    'chat.adversarialRoundSpeech': `Round ${round} response`,
    'chat.adversarialRoundJudge': `Round ${round} judge`,
    'chat.adversarialResolved': 'Resolved',
    'chat.adversarialNotResolved': 'Not resolved',
  };
  return values[key] ?? key;
}) as TFunction;

describe('Super Adversarial timeline model labels', () => {
  it('shows the producing model on every response and the winner on the final answer', () => {
    const run: ChatAdversarialRun = {
      id: 'run-1',
      iteration_no: 1,
      question: 'A or B?',
      models: ['model-a', 'model-b'],
      judge_model: 'model-a',
      status: 'completed',
      current_round: 1,
      max_rounds: 1,
      winner_model: 'model-b',
      winner_reason: 'stronger evidence',
      final_answer: 'Choose B.',
      trace: {
        rounds: [
          {
            round: 1,
            phase: 'initial',
            answers: [
              { model: 'model-a', answer: 'Choose A.' },
              { model: 'model-b', answer: 'Choose B.' },
            ],
            judge: {
              resolved: true,
              winnerModel: 'model-b',
              winnerReason: 'stronger evidence',
            },
          },
        ],
      },
      created_at: '2026-07-22T12:00:00Z',
      updated_at: '2026-07-22T12:01:00Z',
      completed_at: '2026-07-22T12:01:00Z',
    };

    const timeline = buildRunTimeline(run, t);
    const modelReplies = timeline.filter((item) => item.role === 'model');
    const judge = timeline.find((item) => item.role === 'judge');
    const final = timeline.find((item) => item.role === 'final');

    expect(modelReplies.map((item) => item.title)).toEqual(['model-a', 'model-b']);
    expect(judge).toMatchObject({ title: 'Judge model: model-a', model: 'model-a' });
    expect(final).toMatchObject({
      title: 'Final answer · Synthesis model: model-a',
      subtitle: 'Winner: model-b',
      model: 'model-a',
    });
  });
});
