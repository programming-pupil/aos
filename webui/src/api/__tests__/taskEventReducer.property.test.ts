import { describe, expect, it } from 'vitest';
import fc from 'fast-check';
import { initialTaskEventState, reduceTaskEvent } from '../taskEventReducer';
import type { TaskEvent } from '../tasks';

function event(id: number, taskId = 'task-1', stateVersion = id): TaskEvent {
  return {
    id,
    eventId: `event-${id}`,
    taskId,
    rootTaskId: taskId,
    eventType: 'task.progress',
    stateVersion,
    visibility: 'owner',
    payload: { message: `event ${id}` },
    createdAt: '2026-07-25 00:00:00.000',
  };
}

describe('task event reducer', () => {
  it('deduplicates replayed events and preserves ascending timeline order', () => {
    fc.assert(
      fc.property(
        fc.uniqueArray(fc.integer({ min: 1, max: 10_000 }), {
          minLength: 1,
          maxLength: 100,
        }),
        (ids) => {
          const replayed = [...ids].reverse().flatMap((id) => [id, id]);
          const state = replayed.reduce(
            (current, id) => reduceTaskEvent(current, event(id)),
            initialTaskEventState(),
          );
          expect(state.events.map((item) => item.id)).toEqual([...ids].sort((a, b) => a - b));
          expect(state.lastEventId).toBe(Math.max(...ids));
        },
      ),
      { numRuns: 100 },
    );
  });

  it('never lets an older state version replace the latest task version', () => {
    fc.assert(
      fc.property(
        fc.array(fc.integer({ min: 0, max: 5_000 }), { minLength: 1, maxLength: 100 }),
        (versions) => {
          const state = versions.reduce(
            (current, version, index) =>
              reduceTaskEvent(current, event(index + 1, 'task-1', version)),
            initialTaskEventState(),
          );
          expect(state.latestVersionByTask['task-1']).toBe(Math.max(...versions));
        },
      ),
      { numRuns: 100 },
    );
  });

  it('bounds retained events and per-task version metadata', () => {
    const state = Array.from({ length: 1_000 }, (_, index) => index + 1).reduce(
      (current, id) => reduceTaskEvent(current, event(id, `task-${id}`)),
      initialTaskEventState(),
    );
    expect(state.events).toHaveLength(500);
    expect(Object.keys(state.seenEventIds)).toHaveLength(500);
    expect(Object.keys(state.latestVersionByTask)).toHaveLength(500);
    expect(state.events[0].id).toBe(501);
    expect(state.lastEventId).toBe(1_000);
  });
});
