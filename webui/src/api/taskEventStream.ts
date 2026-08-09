import type { TaskEvent } from './tasks';

const DEFAULT_MAX_FRAME_CHARS = 1024 * 1024;

export interface TaskEventStreamHandlers {
  onEvent: (event: TaskEvent) => void;
  onWarning?: (message: string) => void;
}

export interface TaskEventStreamParser {
  push: (chunk: string) => void;
  finish: () => void;
}

function isTaskEvent(value: unknown): value is TaskEvent {
  if (!value || typeof value !== 'object') return false;
  const event = value as Partial<TaskEvent>;
  return Number.isSafeInteger(event.id)
    && Number(event.id) >= 0
    && typeof event.eventId === 'string'
    && typeof event.taskId === 'string'
    && typeof event.rootTaskId === 'string'
    && typeof event.eventType === 'string'
    && Number.isSafeInteger(event.stateVersion)
    && typeof event.visibility === 'string'
    && typeof event.createdAt === 'string';
}

export function createTaskEventStreamParser(
  handlers: TaskEventStreamHandlers,
  maxFrameChars = DEFAULT_MAX_FRAME_CHARS,
): TaskEventStreamParser {
  let buffer = '';
  let eventName = 'message';
  let dataLines: string[] = [];
  let frameChars = 0;
  let discardingFrame = false;

  const warn = (message: string) => handlers.onWarning?.(message);

  const resetFrame = () => {
    eventName = 'message';
    dataLines = [];
    frameChars = 0;
  };

  const discardOversizedFrame = () => {
    if (!discardingFrame) warn('Ignored an oversized task event stream frame');
    discardingFrame = true;
    resetFrame();
  };

  const dispatchFrame = () => {
    if (discardingFrame) {
      discardingFrame = false;
      resetFrame();
      return;
    }
    if (dataLines.length === 0) {
      resetFrame();
      return;
    }

    const data = dataLines.join('\n');
    const type = eventName;
    resetFrame();
    try {
      const parsed = JSON.parse(data) as unknown;
      if (type === 'task_event') {
        if (isTaskEvent(parsed)) handlers.onEvent(parsed);
        else warn('Ignored an invalid task event stream frame');
      } else if (type === 'stream_warning') {
        const message = parsed && typeof parsed === 'object'
          ? (parsed as { message?: unknown }).message
          : undefined;
        warn(typeof message === 'string' && message.trim()
          ? message
          : 'Task stream temporarily unavailable');
      }
    } catch {
      warn('Ignored a malformed task event stream frame');
    }
  };

  const processLine = (line: string) => {
    if (line === '') {
      dispatchFrame();
      return;
    }
    if (discardingFrame || line.startsWith(':')) return;

    frameChars += line.length;
    if (frameChars > maxFrameChars) {
      discardOversizedFrame();
      return;
    }

    const colon = line.indexOf(':');
    const field = colon < 0 ? line : line.slice(0, colon);
    let value = colon < 0 ? '' : line.slice(colon + 1);
    if (value.startsWith(' ')) value = value.slice(1);
    if (field === 'event') eventName = value;
    else if (field === 'data') dataLines.push(value);
  };

  const drain = (atEnd: boolean) => {
    while (buffer.length > 0) {
      let boundary = -1;
      for (let index = 0; index < buffer.length; index += 1) {
        if (buffer[index] === '\n' || buffer[index] === '\r') {
          boundary = index;
          break;
        }
      }
      if (boundary < 0) break;
      if (buffer[boundary] === '\r' && boundary === buffer.length - 1 && !atEnd) break;

      const line = buffer.slice(0, boundary);
      const separatorLength = buffer[boundary] === '\r' && buffer[boundary + 1] === '\n' ? 2 : 1;
      buffer = buffer.slice(boundary + separatorLength);
      processLine(line);
    }

    if (atEnd && buffer.length > 0) {
      processLine(buffer);
      buffer = '';
    } else if (buffer.length > maxFrameChars) {
      buffer = '';
      discardOversizedFrame();
    }
  };

  return {
    push(chunk: string) {
      if (!chunk) return;
      buffer += chunk;
      drain(false);
    },
    finish() {
      drain(true);
    },
  };
}
