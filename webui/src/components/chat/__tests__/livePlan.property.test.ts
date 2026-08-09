import fc from 'fast-check';
import { describe, expect, it } from 'vitest';
import { deriveLivePlanFromToolCalls, type TodoItem } from '@/components/chat/livePlan';
import type { ToolCallInfo } from '@/components/chat/types';

const statusArb = fc.constantFrom('pending' as const, 'in_progress' as const, 'completed' as const);

const todoArb: fc.Arbitrary<TodoItem> = fc.record({
  content: fc.string({ minLength: 1, maxLength: 80 }).filter((value) => value.trim().length > 0),
  activeForm: fc.string({ minLength: 1, maxLength: 80 }),
  status: statusArb,
});

function todoWriteCall(index: number, todos: TodoItem[]): ToolCallInfo {
  return {
    index,
    name: 'TodoWrite',
    source: 'builtin',
    args: JSON.stringify({ todos }),
    result: JSON.stringify({ oldTodos: [], newTodos: todos, verificationNudgeNeeded: null }),
    isError: false,
    status: 'success',
  };
}

describe('Live Plan property mapping', () => {
  it('faithfully reflects the latest TodoWrite snapshot', () => {
    // Feature: codex-parity-gaps, Property 16: Live_Plan 忠实反映 TodoWrite 步骤与最新状态
    fc.assert(
      fc.property(
        fc.array(fc.array(todoArb, { maxLength: 25 }), { minLength: 1, maxLength: 40 }),
        (snapshots) => {
          const calls = snapshots.map((todos, index) => todoWriteCall(index, todos));
          const view = deriveLivePlanFromToolCalls(calls);
          const latest = snapshots[snapshots.length - 1];

          expect(view.steps).toHaveLength(latest.length);
          for (const [index, todo] of latest.entries()) {
            expect(view.steps[index]).toEqual({
              id: `step-${index}`,
              title: todo.content,
              status: todo.status,
            });
          }
        },
      ),
      { numRuns: 100 },
    );
  });
});
