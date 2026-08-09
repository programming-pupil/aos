// ── Live Plan mapping (codex-parity-gaps, task 6.1) ─────────────────────────────
//
// Maps the session's existing `TodoWrite` output (`TodoItem[]`) and Plan Mode
// state (`EnterPlanMode` / `ExitPlanMode`) onto a `LivePlanView` for incremental
// rendering in the webui. This is a pure, orchestration-only mapping — it reuses
// the runtime's `TodoWrite` / `PlanModeState` semantics and introduces NO parallel
// plan mechanism (Req 4.1 / 4.2 / 4.3, design "Live Plan + 易读性规范").
//
// Data sources (real symbols from rust/crates/tools/src/lib.rs):
//   - `TodoWrite` tool → output `{ oldTodos, newTodos, verificationNudgeNeeded }`,
//     each `TodoItem = { content, activeForm, status }` where status is one of
//     `pending | in_progress | completed` (serde snake_case).
//   - `EnterPlanMode` / `ExitPlanMode` tools → `PlanModeOutput { active, ... }`;
//     `planModeActive` derives from the latest plan-mode tool result (Req 4.3).
//
// TodoWrite always emits the FULL updated list on every call, so the latest
// snapshot is the current plan state. Mapping the latest snapshot therefore
// yields steps that correspond one-to-one to the source `TodoItem`s and reflect
// each step's most recent status (design Property 16).

import type { ToolCallInfo } from './types';

/** Step status, mirroring the runtime `TodoStatus` enum (serde snake_case). */
export type PlanStepStatus = 'pending' | 'in_progress' | 'completed';

/** A single Live Plan step derived from a `TodoItem` (design data model). */
export interface PlanStep {
  id: string;
  title: string;
  status: PlanStepStatus;
}

/** Live Plan view rendered in the webui (design data model, Req 4.1/4.2). */
export interface LivePlanView {
  /** One-to-one with the source `TodoItem`s, in list order. */
  steps: PlanStep[];
  /** Derived from the latest Plan Mode (`EnterPlanMode`/`ExitPlanMode`) state. */
  planModeActive: boolean;
}

/**
 * `TodoItem` as emitted by the `TodoWrite` tool. Field names match the runtime
 * JSON payload (`activeForm` is camelCase; `status` is snake_case).
 */
export interface TodoItem {
  content: string;
  activeForm: string;
  status: PlanStepStatus;
}

const VALID_STATUSES: readonly PlanStepStatus[] = ['pending', 'in_progress', 'completed'];

/** Names of the reused Plan Mode tools (rust/crates/tools/src/lib.rs). */
const ENTER_PLAN_MODE = 'EnterPlanMode';
const EXIT_PLAN_MODE = 'ExitPlanMode';
const TODO_WRITE = 'TodoWrite';

function isPlanStepStatus(value: unknown): value is PlanStepStatus {
  return typeof value === 'string' && (VALID_STATUSES as readonly string[]).includes(value);
}

/** Best-effort JSON parse; tolerates the empty/streaming-in-progress case. */
function safeParseJson(raw: string | undefined | null): unknown {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/** Coerce an unknown value into a well-formed `TodoItem`, or `null` if invalid. */
function coerceTodoItem(value: unknown): TodoItem | null {
  if (!value || typeof value !== 'object') return null;
  const record = value as Record<string, unknown>;
  const content = record.content;
  if (typeof content !== 'string' || content.trim() === '') return null;
  if (!isPlanStepStatus(record.status)) return null;
  const activeForm = typeof record.activeForm === 'string' ? record.activeForm : content;
  return { content, activeForm, status: record.status };
}

/** Normalize an unknown array into valid `TodoItem`s (drops malformed entries). */
export function normalizeTodos(value: unknown): TodoItem[] {
  if (!Array.isArray(value)) return [];
  const out: TodoItem[] = [];
  for (const entry of value) {
    const todo = coerceTodoItem(entry);
    if (todo) out.push(todo);
  }
  return out;
}

/**
 * Map a single `TodoItem` to a `PlanStep`. `TodoItem` carries no explicit id, so
 * a stable positional id is derived from the list index — this preserves the
 * one-to-one correspondence between source steps and rendered steps.
 */
export function mapTodoItemToPlanStep(todo: TodoItem, index: number): PlanStep {
  return {
    id: `step-${index}`,
    title: todo.content,
    status: todo.status,
  };
}

/** Map an ordered `TodoItem` list to `PlanStep`s (one-to-one, in order). */
export function mapTodosToPlanSteps(todos: TodoItem[]): PlanStep[] {
  return todos.map(mapTodoItemToPlanStep);
}

/** Build a `LivePlanView` from the latest todos snapshot and plan-mode flag. */
export function mapToLivePlan(todos: TodoItem[], planModeActive = false): LivePlanView {
  return {
    steps: mapTodosToPlanSteps(todos),
    planModeActive,
  };
}

/**
 * Fold an ordered sequence of `TodoWrite` snapshots into the current view.
 *
 * Every `TodoWrite` call carries the full updated list, so the latest non-empty
 * snapshot is the authoritative current plan and its statuses are the most
 * recent. Supports the "incremental render" requirement without a parallel plan
 * store (Req 4.1/4.2).
 */
export function reduceLivePlan(
  snapshots: TodoItem[][],
  planModeActive = false,
): LivePlanView {
  const latest = snapshots.length > 0 ? snapshots[snapshots.length - 1] : [];
  return mapToLivePlan(latest, planModeActive);
}

/** Extract the `newTodos` snapshot from a `TodoWrite` tool call, if present. */
function extractTodosFromToolCall(tool: ToolCallInfo): TodoItem[] | null {
  if (tool.name !== TODO_WRITE) return null;
  // Prefer the executed result (`newTodos`); fall back to the pending args
  // (`todos`) so the plan renders incrementally while the call is still running.
  const result = safeParseJson(tool.result);
  if (result && typeof result === 'object' && 'newTodos' in result) {
    return normalizeTodos((result as Record<string, unknown>).newTodos);
  }
  const args = safeParseJson(tool.args);
  if (args && typeof args === 'object' && 'todos' in args) {
    return normalizeTodos((args as Record<string, unknown>).todos);
  }
  return null;
}

/**
 * Determine the plan-mode active flag contributed by a single tool call.
 * Returns `null` when the call is not a Plan Mode tool (leaves state unchanged).
 */
function extractPlanModeActive(tool: ToolCallInfo): boolean | null {
  if (tool.name !== ENTER_PLAN_MODE && tool.name !== EXIT_PLAN_MODE) return null;
  // `PlanModeOutput.active` is the source of truth when the tool has executed.
  const result = safeParseJson(tool.result);
  if (result && typeof result === 'object' && typeof (result as Record<string, unknown>).active === 'boolean') {
    return (result as Record<string, unknown>).active as boolean;
  }
  // Fall back to tool semantics: entering activates, exiting deactivates.
  return tool.name === ENTER_PLAN_MODE;
}

/**
 * Derive the current `LivePlanView` from a session's tool-call stream.
 *
 * Scans the calls in `index` order, keeping the latest `TodoWrite` snapshot and
 * the latest Plan Mode state. Reuses the existing `TodoWrite` / `EnterPlanMode` /
 * `ExitPlanMode` outputs — no new data source is introduced (Req 4.3).
 */
export function deriveLivePlanFromToolCalls(toolCalls: ToolCallInfo[]): LivePlanView {
  const ordered = [...toolCalls].sort((a, b) => a.index - b.index);
  let latestTodos: TodoItem[] = [];
  let planModeActive = false;

  for (const tool of ordered) {
    const todos = extractTodosFromToolCall(tool);
    if (todos !== null) latestTodos = todos;

    const active = extractPlanModeActive(tool);
    if (active !== null) planModeActive = active;
  }

  return mapToLivePlan(latestTodos, planModeActive);
}
