// ── Live Plan mapping unit tests (codex-parity-gaps, task 6.1) ──────────────────
//
// Verifies the LivePlanView / PlanStep mapping reuses the existing TodoWrite
// output and Plan Mode (EnterPlanMode / ExitPlanMode) state, mapping steps
// one-to-one with the latest TodoItem statuses (Req 4.1 / 4.2 / 4.3).
//
// These are example/edge-case unit tests. The numbered property test
// (Property 16, fast-check) is implemented separately by task 6.4.

import { describe, expect, it } from 'vitest';
import {
  deriveLivePlanFromToolCalls,
  mapToLivePlan,
  mapTodosToPlanSteps,
  normalizeTodos,
  reduceLivePlan,
  type TodoItem,
} from '@/components/chat/livePlan';
import type { ToolCallInfo } from '@/components/chat/types';

function todoCall(index: number, newTodos: unknown, status: ToolCallInfo['status'] = 'success'): ToolCallInfo {
  return {
    index,
    name: 'TodoWrite',
    source: 'builtin',
    args: JSON.stringify({ todos: newTodos }),
    result: JSON.stringify({ oldTodos: [], newTodos, verificationNudgeNeeded: null }),
    isError: false,
    status,
  };
}

function planModeCall(index: number, name: 'EnterPlanMode' | 'ExitPlanMode', active: boolean): ToolCallInfo {
  return {
    index,
    name,
    source: 'builtin',
    args: '{}',
    result: JSON.stringify({ success: true, operation: name, active }),
    isError: false,
    status: 'success',
  };
}

const SAMPLE: TodoItem[] = [
  { content: 'Read spec', activeForm: 'Reading spec', status: 'completed' },
  { content: 'Write code', activeForm: 'Writing code', status: 'in_progress' },
  { content: 'Verify build', activeForm: 'Verifying build', status: 'pending' },
];

describe('mapTodosToPlanSteps', () => {
  it('maps each TodoItem one-to-one preserving order, title and status', () => {
    const steps = mapTodosToPlanSteps(SAMPLE);
    expect(steps).toEqual([
      { id: 'step-0', title: 'Read spec', status: 'completed' },
      { id: 'step-1', title: 'Write code', status: 'in_progress' },
      { id: 'step-2', title: 'Verify build', status: 'pending' },
    ]);
  });

  it('produces an empty plan for an empty todo list', () => {
    expect(mapToLivePlan([])).toEqual({ steps: [], planModeActive: false });
  });
});

describe('normalizeTodos', () => {
  it('drops malformed entries and invalid statuses', () => {
    const raw = [
      { content: 'ok', activeForm: 'doing', status: 'pending' },
      { content: '', activeForm: 'x', status: 'pending' }, // empty content
      { content: 'bad status', activeForm: 'x', status: 'done' }, // invalid status
      42,
      null,
    ];
    expect(normalizeTodos(raw)).toEqual([
      { content: 'ok', activeForm: 'doing', status: 'pending' },
    ]);
  });

  it('defaults activeForm to content when absent', () => {
    expect(normalizeTodos([{ content: 'task', status: 'pending' }])).toEqual([
      { content: 'task', activeForm: 'task', status: 'pending' },
    ]);
  });
});

describe('reduceLivePlan', () => {
  it('keeps the latest snapshot (TodoWrite emits the full list each call)', () => {
    const first: TodoItem[] = [{ content: 'A', activeForm: 'doing A', status: 'pending' }];
    const second: TodoItem[] = [{ content: 'A', activeForm: 'doing A', status: 'completed' }];
    const view = reduceLivePlan([first, second], true);
    expect(view.steps).toEqual([{ id: 'step-0', title: 'A', status: 'completed' }]);
    expect(view.planModeActive).toBe(true);
  });
});

describe('deriveLivePlanFromToolCalls', () => {
  it('uses the latest TodoWrite newTodos snapshot regardless of call order', () => {
    const older = todoCall(0, [{ content: 'A', activeForm: 'doing A', status: 'pending' }]);
    const newer = todoCall(2, [{ content: 'A', activeForm: 'doing A', status: 'completed' }]);
    // Provide out of order; derivation sorts by index.
    const view = deriveLivePlanFromToolCalls([newer, older]);
    expect(view.steps).toEqual([{ id: 'step-0', title: 'A', status: 'completed' }]);
  });

  it('falls back to pending args.todos while the TodoWrite call is running', () => {
    const running: ToolCallInfo = {
      index: 0,
      name: 'TodoWrite',
      source: 'builtin',
      args: JSON.stringify({ todos: [{ content: 'A', activeForm: 'doing A', status: 'in_progress' }] }),
      result: '',
      isError: false,
      status: 'running',
    };
    const view = deriveLivePlanFromToolCalls([running]);
    expect(view.steps).toEqual([{ id: 'step-0', title: 'A', status: 'in_progress' }]);
  });

  it('reflects Plan Mode via EnterPlanMode / ExitPlanMode tool results', () => {
    const enter = planModeCall(0, 'EnterPlanMode', true);
    const todos = todoCall(1, SAMPLE);
    const exit = planModeCall(2, 'ExitPlanMode', false);

    expect(deriveLivePlanFromToolCalls([enter, todos]).planModeActive).toBe(true);
    expect(deriveLivePlanFromToolCalls([enter, todos, exit]).planModeActive).toBe(false);
  });

  it('returns an empty inactive plan when there is no TodoWrite data', () => {
    expect(deriveLivePlanFromToolCalls([])).toEqual({ steps: [], planModeActive: false });
  });
});
