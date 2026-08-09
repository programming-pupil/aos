import type { TaskEvent } from './tasks';

export interface TaskEventState {
  lastEventId: number;
  events: TaskEvent[];
  seenEventIds: Record<string, true>;
  latestVersionByTask: Record<string, number>;
}

const MAX_RETAINED_EVENTS = 500;

export function initialTaskEventState(afterEventId = 0): TaskEventState {
  return {
    lastEventId: Math.max(0, afterEventId),
    events: [],
    seenEventIds: {},
    latestVersionByTask: {},
  };
}

export function reduceTaskEvent(state: TaskEventState, event: TaskEvent): TaskEventState {
  const eventKey = event.eventId || `seq:${event.id}`;
  if (state.seenEventIds[eventKey] || state.events.some((item) => item.id === event.id)) {
    return state;
  }
  const events = [...state.events, event]
    .sort((left, right) => left.id - right.id)
    .slice(-MAX_RETAINED_EVENTS);
  const retainedKeys: Record<string, true> = {};
  const latestVersionByTask: Record<string, number> = {};
  for (const item of events) {
    retainedKeys[item.eventId || `seq:${item.id}`] = true;
    latestVersionByTask[item.taskId] = Math.max(
      state.latestVersionByTask[item.taskId] ?? 0,
      latestVersionByTask[item.taskId] ?? 0,
      item.stateVersion,
    );
  }
  return {
    lastEventId: Math.max(state.lastEventId, event.id),
    events,
    seenEventIds: retainedKeys,
    latestVersionByTask,
  };
}

export function taskEventMessage(event: TaskEvent): string {
  const message = event.payload?.message?.trim();
  if (message) return message;
  return event.eventType.replace(/^task\./, '').replaceAll('_', ' ');
}

export function taskEventsForRoot(state: TaskEventState, rootTaskId: string): TaskEvent[] {
  return state.events.filter(
    (event) => event.rootTaskId === rootTaskId || event.taskId === rootTaskId,
  );
}
