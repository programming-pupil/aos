export interface SuperAssistantEventState {
  lastEventId: number;
  terminal: boolean;
  finalText?: string;
  textDeltas: string[];
  thinkingDeltas: string[];
  usage?: unknown;
  pmQuality?: unknown;
  pmReport?: Record<string, unknown>;
}

export type SuperAssistantEventEffect =
  | { type: 'route'; turnId?: string }
  | { type: 'session_activated' | 'config_hot_reload'; value: unknown }
  | { type: 'session_compacted'; removedMessages: number; summary: string }
  | { type: 'thinking_start' | 'thinking_end' | 'text_block_start' | 'text_block_end'; index: number }
  | { type: 'thinking_delta' | 'commentary_delta' | 'text_delta'; text: string }
  | { type: 'tool_start'; index: number; id: string; name: string }
  | { type: 'tool_input'; index: number; input: string }
  | { type: 'tool_end'; index: number }
  | {
      type: 'tool_result';
      index: number;
      toolName: string;
      input: string;
      output: string;
      isError: boolean;
      durationMs?: number;
    }
  | { type: 'tool_call'; value: unknown }
  | { type: 'usage' | 'pm_quality' | 'pm_report' | 'super_assistant_answer'; value: unknown }
  | { type: 'pm_stage'; value: Record<string, unknown> }
  | { type: 'image_context_warning'; value: Record<string, unknown> }
  | {
      type: 'stream_end';
      iterations: number;
      fullText: string;
      thinking: string;
      streamMode?: string;
      telemetry?: unknown;
    }
  | { type: 'error'; message: string };

export interface SuperAssistantSseEvent {
  id: number;
  event: string;
  data: unknown;
}

export interface DataAttributionTaskBinding {
  taskId: string;
  status: string;
}

export function dataAttributionTaskBindingFromStage(
  stage: unknown,
): DataAttributionTaskBinding | null {
  const stageObject = objectValue(stage);
  const detail = objectValue(stageObject.detail);
  if (detail.engine !== 'data_attribution') return null;
  const taskId = typeof detail.externalTaskId === 'string'
    ? detail.externalTaskId.trim()
    : '';
  if (!taskId) return null;
  const status = typeof detail.sourceStatus === 'string'
    ? detail.sourceStatus
    : typeof stageObject.status === 'string'
      ? stageObject.status
      : 'running';
  return { taskId, status };
}

export function createSuperAssistantEventState(afterEventId = 0): SuperAssistantEventState {
  return {
    lastEventId: Math.max(0, afterEventId),
    terminal: false,
    textDeltas: [],
    thinkingDeltas: [],
  };
}

function objectValue(value: unknown): Record<string, any> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, any>)
    : {};
}

function unwrapData(value: unknown): any {
  const object = objectValue(value);
  return Object.prototype.hasOwnProperty.call(object, 'data') ? object.data : value;
}

function wrappedValue(value: unknown, key: string): any {
  const object = objectValue(value);
  return Object.prototype.hasOwnProperty.call(object, key) ? object[key] : unwrapData(value);
}

function textValue(value: unknown, ...keys: string[]): string {
  if (typeof value === 'string') return value;
  const object = objectValue(value);
  for (const key of keys) {
    if (typeof object[key] === 'string') return object[key];
  }
  return '';
}

function serializableText(value: unknown): string {
  if (typeof value === 'string') return value;
  if (value == null) return '';
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function indexValue(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) return value;
  const index = objectValue(value).index;
  return typeof index === 'number' && Number.isFinite(index) ? index : 0;
}

function stageStatus(event: string): string {
  if (event.endsWith('_failed') || event === 'verification_failed') return 'failed';
  if (
    event.endsWith('_completed')
    || event === 'verification_passed'
    || event === 'verification_degraded'
  ) return 'completed';
  if (event.endsWith('_cancelled')) return 'cancelled';
  return 'running';
}

function resolvedText(state: SuperAssistantEventState): string {
  const deltas = state.textDeltas.join('');
  if (!state.finalText) return deltas;
  if (deltas.length > state.finalText.length && deltas.endsWith(state.finalText)) return deltas;
  return state.finalText;
}

function unseenRecoveryCheckpointText(
  currentText: string,
  checkpointText: string,
  data: Record<string, any>,
): string {
  if (data.recoveryCheckpoint !== true) return checkpointText;
  const start = data.checkpointStart;
  const end = data.checkpointEnd;
  if (
    typeof start !== 'number'
    || !Number.isSafeInteger(start)
    || start < 0
    || typeof end !== 'number'
    || !Number.isSafeInteger(end)
    || end < start
    || end - start !== checkpointText.length
  ) {
    return checkpointText;
  }
  if (currentText.length <= start) return checkpointText;
  const visiblePart = currentText.slice(start, Math.min(currentText.length, end));
  if (!checkpointText.startsWith(visiblePart)) return checkpointText;
  return checkpointText.slice(visiblePart.length);
}

export function reduceSuperAssistantEvent(
  state: SuperAssistantEventState,
  event: SuperAssistantSseEvent,
): { state: SuperAssistantEventState; effects: SuperAssistantEventEffect[] } {
  if (event.id > 0 && event.id <= state.lastEventId) return { state, effects: [] };
  if (state.terminal) return { state, effects: [] };

  const next: SuperAssistantEventState = {
    ...state,
    lastEventId: event.id > 0 ? Math.max(state.lastEventId, event.id) : state.lastEventId,
    textDeltas: [...state.textDeltas],
    thinkingDeltas: [...state.thinkingDeltas],
  };
  const effects: SuperAssistantEventEffect[] = [];
  const data = unwrapData(event.data);
  const object = objectValue(data);

  switch (event.event) {
    case 'ping':
      break;
    case 'route_decision':
      effects.push({ type: 'route', turnId: object.turnId ?? object.turn_id });
      break;
    case 'session_activated':
      effects.push({ type: 'session_activated', value: wrappedValue(event.data, 'SessionActivated') });
      break;
    case 'config_hot_reload':
      effects.push({ type: 'config_hot_reload', value: wrappedValue(event.data, 'ConfigHotReload') });
      break;
    case 'session_compacted':
      effects.push({
        type: 'session_compacted',
        removedMessages: object.removed_messages ?? object.removedMessages ?? 0,
        summary: object.summary ?? '',
      });
      break;
    case 'thinking_start':
    case 'thinking_end':
    case 'text_block_start':
    case 'text_block_end':
      effects.push({ type: event.event, index: indexValue(data) });
      break;
    case 'thinking_delta':
    case 'thinking': {
      const text = textValue(data, 'text', 'thinking');
      if (text) {
        next.thinkingDeltas.push(text);
        effects.push({ type: 'thinking_delta', text });
      }
      break;
    }
    case 'text_delta': {
      const text = textValue(data, 'text');
      if (text) {
        next.textDeltas.push(text);
        effects.push({ type: 'text_delta', text });
      }
      break;
    }
    case 'commentary_delta': {
      const text = textValue(data, 'text');
      if (text) effects.push({ type: 'commentary_delta', text });
      break;
    }
    case 'final_delta': {
      const text = textValue(data, 'text', 'delta');
      if (!text) break;

      // Unified parent direct answers arrive as persisted streaming deltas.
      // A terminal specialist answer is instead one authoritative snapshot.
      // Older events did not carry `mode`: an indexed event was a runtime
      // delta, while an unindexed event was the terminal snapshot.
      const mode = object.mode ?? (typeof object.index === 'number' ? 'delta' : 'snapshot');
      if (mode === 'delta') {
        const unseenText = unseenRecoveryCheckpointText(next.textDeltas.join(''), text, object);
        if (unseenText) {
          next.textDeltas.push(unseenText);
          effects.push({ type: 'text_delta', text: unseenText });
        }
      } else {
        next.finalText = text;
      }
      break;
    }
    case 'text': {
      const text = textValue(data, 'text', 'delta');
      if (text) next.finalText = text;
      break;
    }
    case 'tool_use_start':
    case 'tool_started':
      effects.push({
        type: 'tool_start',
        index: indexValue(data) || event.id,
        id: object.id ?? object.toolCallId ?? `tool-${event.id}`,
        name: object.name ?? object.tool ?? '',
      });
      break;
    case 'tool_use_input':
      effects.push({ type: 'tool_input', index: indexValue(data), input: object.input ?? '' });
      break;
    case 'tool_use_end':
      effects.push({ type: 'tool_end', index: indexValue(data) });
      break;
    case 'tool_result':
    case 'tool_completed':
    case 'tool_failed':
      effects.push({
        type: 'tool_result',
        index: indexValue(data) || event.id,
        toolName: object.tool_name ?? object.tool ?? '',
        input: serializableText(object.input),
        output: serializableText(object.output),
        isError: object.is_error ?? object.isError ?? event.event === 'tool_failed',
        durationMs: object.duration_ms ?? object.durationMs,
      });
      break;
    case 'tool_call':
      effects.push({ type: 'tool_call', value: data });
      break;
    case 'usage':
      next.usage = data;
      effects.push({ type: 'usage', value: data });
      break;
    case 'pm_quality':
      next.pmQuality = data;
      effects.push({ type: 'pm_quality', value: data });
      break;
    case 'pm_report':
      if (data && typeof data === 'object' && !Array.isArray(data)) {
        next.pmReport = data as Record<string, unknown>;
        effects.push({ type: 'pm_report', value: data });
      }
      break;
    case 'pm_stage':
      effects.push({ type: 'pm_stage', value: object });
      break;
    case 'commentary':
    case 'turn_started':
    case 'turn_model_started':
    case 'turn_waiting_subagent':
    case 'subtask_started':
    case 'subtask_progress':
    case 'subtask_completed':
    case 'subtask_failed':
    case 'verification_passed':
    case 'verification_degraded':
    case 'turn_completed':
    case 'turn_cancelled':
      effects.push({
        type: 'pm_stage',
        value: {
          stage: object.stage ?? event.event,
          status: object.status ?? stageStatus(event.event),
          detail: object,
        },
      });
      break;
    case 'verification_failed':
      effects.push({
        type: 'pm_stage',
        value: {
          stage: 'verification_repair',
          status: 'running',
          detail: object,
        },
      });
      break;
    case 'image_context_warning':
      effects.push({ type: 'image_context_warning', value: object });
      break;
    case 'super_assistant_answer':
      effects.push({ type: 'super_assistant_answer', value: data });
      break;
    case 'stream_end':
      next.terminal = true;
      effects.push({
        type: 'stream_end',
        iterations: object.iterations ?? 0,
        fullText: resolvedText(next),
        thinking: next.thinkingDeltas.join(''),
        streamMode: object.streamMode ?? object.stream_mode,
        telemetry: object.telemetry,
      });
      break;
    case 'turn_failed':
    case 'error':
      next.terminal = true;
      effects.push({
        type: 'error',
        message: object.error ?? object.message ?? '未知错误',
      });
      break;
    default:
      break;
  }
  return { state: next, effects };
}
