import dayjs from 'dayjs';
import type { RdTaskEvent } from '@/types';
import { RD_STALE_RUNTIME_TOOL_MS } from './constants';
import type { RdTimelineEvent, RuntimeToolCall } from './types';

export function asRuntimeToolCalls(value: unknown): RuntimeToolCall[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is RuntimeToolCall => !!item && typeof item === 'object');
}

function firstRuntimeToolCall(event: RdTaskEvent): RuntimeToolCall | null {
  return asRuntimeToolCalls(event.detailJson?.toolCalls)[0] ?? null;
}

function runtimeToolEventName(event: RdTaskEvent): string | undefined {
  const call = firstRuntimeToolCall(event);
  if (typeof call?.toolName === 'string' && call.toolName.trim()) return call.toolName.trim();
  const detail = event.detailJson;
  if (detail && typeof detail === 'object') {
    const toolName = detail.toolName;
    if (typeof toolName === 'string' && toolName.trim()) return toolName.trim();
  }
  return undefined;
}

function runtimeToolEventIndex(event: RdTaskEvent): number | undefined {
  const call = firstRuntimeToolCall(event);
  if (typeof call?.index === 'number') return call.index;
  const detail = event.detailJson;
  if (detail && typeof detail === 'object') {
    const index = detail.index;
    if (typeof index === 'number') return index;
  }
  return undefined;
}

function runtimeToolMergeKey(event: RdTaskEvent): string | null {
  if (event.stage !== 'runtime_tool') return null;
  const name = runtimeToolEventName(event);
  const index = runtimeToolEventIndex(event);
  if (!name || typeof index !== 'number') return null;
  return `${index}:${name}`;
}

function parseMaybeJsonObject(value?: string): Record<string, unknown> | null {
  const trimmed = value?.trim();
  if (!trimmed || (!trimmed.startsWith('{') && !trimmed.startsWith('['))) return null;
  try {
    const parsed = JSON.parse(trimmed);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

function stringField(value: Record<string, unknown> | null, keys: string[]): string | null {
  if (!value) return null;
  for (const key of keys) {
    const candidate = value[key];
    if (typeof candidate === 'string' && candidate.trim()) return candidate.trim();
  }
  return null;
}

export function runtimeToolTargetLabel(event: RdTimelineEvent | RdTaskEvent): string | null {
  const call = firstRuntimeToolCall(event);
  if (typeof call?.target === 'string' && call.target.trim()) return call.target.trim();
  const detail = event.detailJson;
  if (detail && typeof detail === 'object' && typeof detail.target === 'string' && detail.target.trim()) {
    return detail.target.trim();
  }
  const toolName = runtimeToolEventName(event);
  const inputObject = parseMaybeJsonObject(call?.input);
  const path = stringField(inputObject, ['path', 'file_path', 'filePath', 'target_path', 'targetPath']);
  if (path) return path;
  const command = stringField(inputObject, ['command', 'cmd']);
  if (command) return command.length > 140 ? `${command.slice(0, 140)}...` : command;
  const pattern = stringField(inputObject, ['pattern', 'query', 'glob']);
  if (pattern) return pattern;
  const url = stringField(inputObject, ['url', 'href']);
  if (url) return url;
  if (typeof call?.input === 'string' && call.input.trim()) {
    const compact = call.input.trim().replace(/\s+/g, ' ');
    if (compact.length <= 120 && !compact.startsWith('{')) return compact;
  }
  return toolName ?? null;
}

export function runtimeToolReasonLabel(call: RuntimeToolCall): string | null {
  return typeof call.reason === 'string' && call.reason.trim() ? call.reason.trim() : null;
}

export function mergeRuntimeToolTimelineEvents(events: RdTaskEvent[]): RdTimelineEvent[] {
  const chronological = [...events].sort((a, b) => a.id - b.id);
  const merged: RdTimelineEvent[] = [];
  const activeRuntimeTools = new Map<string, number>();

  for (const event of chronological) {
    const detailEvent = event.detailJson?.event;
    const mergeKey = runtimeToolMergeKey(event);
    if (event.stage === 'runtime_tool' && mergeKey && detailEvent === 'tool_use_start') {
      const startEvent: RdTimelineEvent = {
        ...event,
        displayStartedAt: event.createdAt,
        displayToolTarget: runtimeToolTargetLabel(event) ?? undefined,
      };
      activeRuntimeTools.set(mergeKey, merged.length);
      merged.push(startEvent);
      continue;
    }

    if (event.stage === 'runtime_tool' && mergeKey && detailEvent === 'tool_use_input') {
      const startIndex = activeRuntimeTools.get(mergeKey);
      const call = firstRuntimeToolCall(event);
      if (typeof startIndex === 'number') {
        const startEvent = merged[startIndex];
        merged[startIndex] = {
          ...startEvent,
          message: event.message || startEvent.message,
          detailJson: {
            ...(startEvent.detailJson ?? {}),
            toolInputEventId: event.id,
            toolCalls: call ? [{
              ...call,
              durationMs: 0,
            }] : startEvent.detailJson?.toolCalls,
          },
          displayToolTarget: runtimeToolTargetLabel(event) ?? startEvent.displayToolTarget,
        };
        continue;
      }

      merged.push({
        ...event,
        displayToolTarget: runtimeToolTargetLabel(event) ?? undefined,
      });
      continue;
    }

    if (event.stage === 'runtime_tool' && mergeKey && detailEvent === 'tool_result') {
      const startIndex = activeRuntimeTools.get(mergeKey);
      const call = firstRuntimeToolCall(event);
      const completedAt = event.createdAt;
      if (typeof startIndex === 'number') {
        const startEvent = merged[startIndex];
        const durationMs = Math.max(
          0,
          dayjs(completedAt).valueOf() - dayjs(startEvent.displayStartedAt ?? startEvent.createdAt).valueOf(),
        );
        merged[startIndex] = {
          ...event,
          detailJson: {
            ...(event.detailJson ?? {}),
            toolLifecycle: {
              startEventId: startEvent.id,
              resultEventId: event.id,
              startedAt: startEvent.displayStartedAt ?? startEvent.createdAt,
              completedAt,
              durationMs,
            },
            toolCalls: call ? [{
              ...call,
              durationMs: call.durationMs && call.durationMs > 0 ? call.durationMs : durationMs,
            }] : event.detailJson?.toolCalls,
          },
          displayStartedAt: startEvent.displayStartedAt ?? startEvent.createdAt,
          displayCompletedAt: completedAt,
          displayDurationMs: durationMs,
          displayToolTarget: runtimeToolTargetLabel(event) ?? startEvent.displayToolTarget,
        };
        activeRuntimeTools.delete(mergeKey);
        continue;
      }

      merged.push({
        ...event,
        displayCompletedAt: completedAt,
        displayToolTarget: runtimeToolTargetLabel(event) ?? undefined,
      });
      continue;
    }

    merged.push(event);
  }

  return merged.sort((a, b) => b.id - a.id);
}

export function isStaleOpenRuntimeTool(event: RdTimelineEvent, nowMs = Date.now()): boolean {
  if (event.stage !== 'runtime_tool' || event.status !== 'running' || event.displayCompletedAt) {
    return false;
  }
  const startedAt = event.displayStartedAt ?? event.createdAt;
  const startedAtMs = dayjs(startedAt).valueOf();
  return Number.isFinite(startedAtMs) && nowMs - startedAtMs > RD_STALE_RUNTIME_TOOL_MS;
}
