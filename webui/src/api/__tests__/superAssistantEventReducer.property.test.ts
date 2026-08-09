import fc from 'fast-check';
import { describe, expect, it } from 'vitest';
import {
  createSuperAssistantEventState,
  dataAttributionTaskBindingFromStage,
  reduceSuperAssistantEvent,
  type SuperAssistantEventEffect,
} from '@/api/superAssistantEventReducer';

describe('Super Assistant event reducer', () => {
  it('deduplicates every persisted event id across arbitrary replay counts', () => {
    // Feature: unified-agent-workspace, Property: durable event replay is idempotent.
    fc.assert(
      fc.property(
        fc.uniqueArray(fc.integer({ min: 1, max: 100_000 }), {
          minLength: 1,
          maxLength: 120,
        }),
        fc.integer({ min: 1, max: 6 }),
        (ids, replayCount) => {
          const ordered = [...ids].sort((left, right) => left - right);
          let state = createSuperAssistantEventState();
          let effectCount = 0;
          for (let replay = 0; replay < replayCount; replay += 1) {
            for (const id of ordered) {
              const reduced = reduceSuperAssistantEvent(state, {
                id,
                event: 'commentary',
                data: { text: `event-${id}` },
              });
              state = reduced.state;
              effectCount += reduced.effects.length;
            }
          }
          expect(effectCount).toBe(ordered.length);
          expect(state.lastEventId).toBe(ordered.at(-1));
        },
      ),
      { numRuns: 128 },
    );
  });

  it('keeps one authoritative final answer across compatibility events', () => {
    // Feature: unified-agent-workspace, Property: final_delta and legacy text never duplicate final output.
    fc.assert(
      fc.property(fc.string({ minLength: 1, maxLength: 2_000 }), (answer) => {
        let state = createSuperAssistantEventState();
        const effects: SuperAssistantEventEffect[] = [];
        for (const event of [
          { id: 1, event: 'final_delta', data: { text: answer } },
          { id: 2, event: 'text', data: { text: answer } },
          { id: 3, event: 'stream_end', data: { iterations: 2 } },
        ]) {
          const reduced = reduceSuperAssistantEvent(state, event);
          state = reduced.state;
          effects.push(...reduced.effects);
        }
        const terminal = effects.filter(
          (effect): effect is Extract<SuperAssistantEventEffect, { type: 'stream_end' }> =>
            effect.type === 'stream_end',
        );
        expect(terminal).toHaveLength(1);
        expect(terminal[0].fullText).toBe(answer);
        expect(effects.filter((effect) => effect.type === 'text_delta')).toHaveLength(0);
      }),
      { numRuns: 128 },
    );
  });

  it('appends persisted final deltas instead of keeping only the last token', () => {
    // Feature: unified-agent-workspace, Property: streamed final deltas preserve the complete answer in order.
    fc.assert(
      fc.property(
        fc.array(fc.string({ minLength: 1, maxLength: 80 }), {
          minLength: 2,
          maxLength: 40,
        }),
        (chunks) => {
          let state = createSuperAssistantEventState();
          const effects: SuperAssistantEventEffect[] = [];
          chunks.forEach((chunk, index) => {
            const reduced = reduceSuperAssistantEvent(state, {
              id: index + 1,
              event: 'final_delta',
              data: { index: 0, mode: 'delta', text: chunk },
            });
            state = reduced.state;
            effects.push(...reduced.effects);
          });
          const completed = reduceSuperAssistantEvent(state, {
            id: chunks.length + 1,
            event: 'stream_end',
            data: { iterations: 1 },
          });
          const terminal = completed.effects.find(
            (effect): effect is Extract<SuperAssistantEventEffect, { type: 'stream_end' }> =>
              effect.type === 'stream_end',
          );
          expect(terminal?.fullText).toBe(chunks.join(''));
          expect(
            effects
              .map((effect) => (effect.type === 'text_delta' ? effect.text : ''))
              .join(''),
          ).toBe(chunks.join(''));
        },
      ),
      { numRuns: 128 },
    );
  });

  it('never lets late events mutate a terminal turn', () => {
    // Feature: unified-agent-workspace, Property: terminal states absorb all late replay events.
    fc.assert(
      fc.property(fc.string({ minLength: 1, maxLength: 500 }), (lateText) => {
        let state = createSuperAssistantEventState();
        for (const event of [
          { id: 1, event: 'final_delta', data: { text: 'accepted' } },
          { id: 2, event: 'stream_end', data: { iterations: 1 } },
        ]) {
          state = reduceSuperAssistantEvent(state, event).state;
        }
        const reduced = reduceSuperAssistantEvent(state, {
          id: 3,
          event: 'final_delta',
          data: { text: lateText },
        });
        expect(reduced.effects).toHaveLength(0);
        expect(reduced.state.finalText).toBe('accepted');
      }),
      { numRuns: 128 },
    );
  });

  it('normalizes parent tool and subtask events into the existing timeline contract', () => {
    let state = createSuperAssistantEventState();
    const tool = reduceSuperAssistantEvent(state, {
      id: 1,
      event: 'tool_completed',
      data: { tool: 'workspace_rg', input: '{"query":"roi"}', output: '3 matches' },
    });
    state = tool.state;
    expect(tool.effects).toEqual([
      expect.objectContaining({ type: 'tool_result', toolName: 'workspace_rg', isError: false }),
    ]);
    const subtask = reduceSuperAssistantEvent(state, {
      id: 2,
      event: 'subtask_progress',
      data: {
        engine: 'deep_research',
        status: 'running',
        stage: 'retrieve',
        externalEventId: 42,
        liveToolEvent: {
          phase: 'start',
          index: 3,
          tool: 'WebSearch',
          target: 'AOS latest release',
        },
      },
    });
    expect(subtask.effects).toEqual([
      expect.objectContaining({
        type: 'pm_stage',
        value: expect.objectContaining({
          stage: 'retrieve',
          status: 'running',
          detail: expect.objectContaining({
            externalEventId: 42,
            liveToolEvent: expect.objectContaining({ target: 'AOS latest release' }),
          }),
        }),
      }),
    ]);
  });

  it('preserves the effective model in parent turn stage events', () => {
    const reduced = reduceSuperAssistantEvent(createSuperAssistantEventState(), {
      id: 1,
      event: 'turn_started',
      data: { turnId: 'turn-1', model: 'gpt-5.5', status: 'running' },
    });
    expect(reduced.effects).toEqual([
      expect.objectContaining({
        type: 'pm_stage',
        value: expect.objectContaining({
          stage: 'turn_started',
          detail: expect.objectContaining({ model: 'gpt-5.5' }),
        }),
      }),
    ]);
  });

  it('preserves durable data-attribution progress for timeline recovery', () => {
    const reduced = reduceSuperAssistantEvent(createSuperAssistantEventState(), {
      id: 9,
      event: 'subtask_progress',
      data: {
        engine: 'data_attribution',
        stage: 'data_attribution_execute_2',
        status: 'completed',
        externalTaskId: 'attribution-1',
        externalEventId: 5,
        message: 'query 2/4 completed',
        progressPercent: 58,
        observation: { title: 'Channel drill-down', rowCount: 12, sqlCount: 1 },
      },
    });

    expect(reduced.effects).toEqual([
      expect.objectContaining({
        type: 'pm_stage',
        value: expect.objectContaining({
          stage: 'data_attribution_execute_2',
          status: 'completed',
          detail: expect.objectContaining({
            externalEventId: 5,
            progressPercent: 58,
            observation: expect.objectContaining({ rowCount: 12, sqlCount: 1 }),
          }),
        }),
      }),
    ]);
  });

  it('binds every parent data-attribution turn to its own external task', () => {
    const first = dataAttributionTaskBindingFromStage({
      stage: 'subtask_started',
      status: 'running',
      detail: {
        engine: 'data_attribution',
        externalTaskId: 'nl2sql-attribution-task-first',
      },
    });
    const second = dataAttributionTaskBindingFromStage({
      stage: 'data_attribution_execute_1',
      status: 'completed',
      detail: {
        engine: 'data_attribution',
        externalTaskId: 'nl2sql-attribution-task-second',
        sourceStatus: 'running',
      },
    });

    expect(first).toEqual({
      taskId: 'nl2sql-attribution-task-first',
      status: 'running',
    });
    expect(second).toEqual({
      taskId: 'nl2sql-attribution-task-second',
      status: 'running',
    });
  });

  it('preserves adversarial judge and winner metadata in the completed subtask event', () => {
    const reduced = reduceSuperAssistantEvent(createSuperAssistantEventState(), {
      id: 1,
      event: 'subtask_completed',
      data: {
        engine: 'super_adversarial',
        externalTaskId: 'chat-adv-run-1',
        status: 'completed',
        result: {
          judgeModel: 'model-a',
          winnerModel: 'model-b',
          winnerReason: 'stronger evidence',
        },
      },
    });
    expect(reduced.effects).toEqual([
      expect.objectContaining({
        type: 'pm_stage',
        value: expect.objectContaining({
          detail: expect.objectContaining({
            externalTaskId: 'chat-adv-run-1',
            result: expect.objectContaining({
              judgeModel: 'model-a',
              winnerModel: 'model-b',
            }),
          }),
        }),
      }),
    ]);
  });

  it('keeps parent commentary out of the authoritative final answer', () => {
    let state = createSuperAssistantEventState();
    const commentary = reduceSuperAssistantEvent(state, {
      id: 0,
      event: 'commentary_delta',
      data: { index: 0, text: '正在检查来源' },
    });
    state = commentary.state;
    expect(commentary.effects).toEqual([
      { type: 'commentary_delta', text: '正在检查来源' },
    ]);
    expect(state.textDeltas).toEqual([]);

    const completed = reduceSuperAssistantEvent(state, {
      id: 1,
      event: 'final_delta',
      data: { text: '最终结论' },
    });
    expect(completed.state.finalText).toBe('最终结论');
    expect(completed.state.textDeltas).toEqual([]);
  });

  it('reconciles recovery checkpoints without duplicating already streamed text', () => {
    let state = createSuperAssistantEventState();
    state = reduceSuperAssistantEvent(state, {
      id: 0,
      event: 'final_delta',
      data: { mode: 'delta', text: '已经显示' },
    }).state;

    const replayed = reduceSuperAssistantEvent(state, {
      id: 7,
      event: 'final_delta',
      data: {
        mode: 'delta',
        text: '已经显示',
        recoveryCheckpoint: true,
        checkpointStart: 0,
        checkpointEnd: 4,
      },
    });

    expect(replayed.effects).toEqual([]);
    expect(replayed.state.textDeltas.join('')).toBe('已经显示');
    expect(replayed.state.lastEventId).toBe(7);
  });

  it('fills only the missing suffix when a recovery checkpoint follows a partial live delta', () => {
    let state = createSuperAssistantEventState();
    state = reduceSuperAssistantEvent(state, {
      id: 0,
      event: 'final_delta',
      data: { mode: 'delta', text: 'partial' },
    }).state;

    const replayed = reduceSuperAssistantEvent(state, {
      id: 8,
      event: 'final_delta',
      data: {
        mode: 'delta',
        text: 'partially recovered',
        recoveryCheckpoint: true,
        checkpointStart: 0,
        checkpointEnd: 19,
      },
    });

    expect(replayed.effects).toEqual([{ type: 'text_delta', text: 'ly recovered' }]);
    expect(replayed.state.textDeltas.join('')).toBe('partially recovered');
  });

  it('renders degraded verification as a completed timeline step without ending the turn early', () => {
    let state = createSuperAssistantEventState();
    const degraded = reduceSuperAssistantEvent(state, {
      id: 1,
      event: 'verification_degraded',
      data: { missing: ['live source'], message: 'best answer retained' },
    });
    state = degraded.state;
    expect(degraded.effects).toEqual([
      expect.objectContaining({
        type: 'pm_stage',
        value: expect.objectContaining({
          stage: 'verification_degraded',
          status: 'completed',
        }),
      }),
    ]);
    expect(state.terminal).toBe(false);

    const final = reduceSuperAssistantEvent(state, {
      id: 2,
      event: 'final_delta',
      data: { mode: 'snapshot', text: '可用答案\n\n注：部分细节待验证。' },
    });
    expect(final.state.finalText).toContain('可用答案');
  });
});
