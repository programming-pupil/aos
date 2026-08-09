import { describe, expect, it } from 'vitest';
import fc from 'fast-check';
import { initialTaskEventState, reduceTaskEvent } from '../taskEventReducer';
import { createTaskEventStreamParser } from '../taskEventStream';
import type { TaskEvent } from '../tasks';

function taskEvent(id: number): TaskEvent {
  return {
    id,
    eventId: `event-${id}`,
    taskId: 'task-1',
    rootTaskId: 'task-1',
    eventType: 'task.progress',
    stateVersion: id,
    visibility: 'owner',
    payload: { message: `event ${id}` },
    createdAt: '2026-07-25 00:00:00.000',
  };
}

function frame(event: TaskEvent, newline = '\n'): string {
  return `id: ${event.id}${newline}event: task_event${newline}data: ${JSON.stringify(event)}${newline}${newline}`;
}

describe('task event SSE parser', () => {
  it('preserves events across arbitrary network chunk boundaries', () => {
    fc.assert(
      fc.property(
        fc.uniqueArray(fc.integer({ min: 1, max: 1000 }), { minLength: 1, maxLength: 20 }),
        fc.array(fc.integer({ min: 1, max: 31 }), { minLength: 1, maxLength: 80 }),
        (ids, chunkSizes) => {
          const source = ids.map((id) => frame(taskEvent(id), '\r\n')).join('');
          const received: TaskEvent[] = [];
          const parser = createTaskEventStreamParser({ onEvent: (event) => received.push(event) });
          let offset = 0;
          let chunkIndex = 0;
          while (offset < source.length) {
            const size = chunkSizes[chunkIndex % chunkSizes.length];
            parser.push(source.slice(offset, offset + size));
            offset += size;
            chunkIndex += 1;
          }
          parser.finish();
          expect(received.map((event) => event.id)).toEqual(ids);
        },
      ),
      { numRuns: 100 },
    );
  });

  it('supports multiple frames, comments, mixed newlines, and multi-line data', () => {
    const first = JSON.stringify(taskEvent(1));
    const split = first.indexOf(',') + 1;
    const received: TaskEvent[] = [];
    const parser = createTaskEventStreamParser({ onEvent: (event) => received.push(event) });
    parser.push(`: heartbeat\r\nevent: task_event\r\ndata: ${first.slice(0, split)}\n`);
    parser.push(`data: ${first.slice(split)}\r\n\r\n${frame(taskEvent(2))}`);
    parser.finish();
    expect(received).toEqual([taskEvent(1), taskEvent(2)]);
  });

  it('reports warning and malformed frames without dropping later valid events', () => {
    const received: TaskEvent[] = [];
    const warnings: string[] = [];
    const parser = createTaskEventStreamParser({
      onEvent: (event) => received.push(event),
      onWarning: (warning) => warnings.push(warning),
    });
    parser.push('event: stream_warning\ndata: {"message":"database catch-up delayed"}\n\n');
    parser.push('event: task_event\ndata: {not-json}\n\n');
    parser.push(frame(taskEvent(3)));
    parser.finish();
    expect(warnings).toEqual([
      'database catch-up delayed',
      'Ignored a malformed task event stream frame',
    ]);
    expect(received).toEqual([taskEvent(3)]);
  });

  it('lets the reducer deduplicate replayed frames after reconnect', () => {
    let state = initialTaskEventState();
    const parser = createTaskEventStreamParser({
      onEvent: (event) => { state = reduceTaskEvent(state, event); },
    });
    parser.push(`${frame(taskEvent(8))}${frame(taskEvent(8))}${frame(taskEvent(9))}`);
    parser.finish();
    expect(state.events.map((event) => event.id)).toEqual([8, 9]);
    expect(state.lastEventId).toBe(9);
  });

  it('discards an oversized frame and resumes at the next boundary', () => {
    const received: TaskEvent[] = [];
    const warnings: string[] = [];
    const parser = createTaskEventStreamParser(
      { onEvent: (event) => received.push(event), onWarning: (warning) => warnings.push(warning) },
      512,
    );
    parser.push(`event: task_event\ndata: ${'x'.repeat(1000)}\n\n`);
    parser.push(frame(taskEvent(4)));
    parser.finish();
    expect(warnings).toContain('Ignored an oversized task event stream frame');
    expect(received).toEqual([taskEvent(4)]);
  });
});
