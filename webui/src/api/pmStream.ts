import { client, fastClient } from './client';
import type { PmResearchTaskEvent, PmResearchTaskStatusResponse } from './pm';

export interface PmResearchTaskAnswerDeltaEvent {
  task_id?: string;
  session_id?: string;
  stage?: string;
  sequence?: number;
  delta?: string;
}

function isPmResearchTaskTerminalEvent(event: PmResearchTaskEvent): boolean {
  const stage = (event.stage ?? '').toLowerCase();
  const status = (event.status ?? '').toLowerCase();
  if (stage === 'done' || stage === 'failed' || stage === 'cancelled') {
    return true;
  }
  return status === 'completed' && !!event.response;
}

function pmTaskStatusToEvent(
  taskId: string,
  snapshot: PmResearchTaskStatusResponse,
): PmResearchTaskEvent {
  return {
    task_id: (snapshot as any).task_id ?? snapshot.taskId ?? taskId,
    session_id: (snapshot as any).session_id ?? snapshot.sessionId ?? '',
    status: snapshot.status,
    stage: snapshot.stage,
    attempt: snapshot.attempt,
    message: snapshot.message,
    elapsed_ms: (snapshot as any).elapsed_ms ?? snapshot.elapsedMs ?? 0,
    stage_elapsed_ms:
      (snapshot as any).stage_elapsed_ms ?? snapshot.stageElapsedMs,
    detail: snapshot.detail,
    response: snapshot.response,
    error: snapshot.error,
  };
}

async function pollPmTaskTerminalEvent(
  taskId: string,
  isAborted: () => boolean,
): Promise<PmResearchTaskEvent | null> {
  const maxAttempts = 40;
  const intervalMs = 1500;
  for (let i = 0; i < maxAttempts; i += 1) {
    if (isAborted()) return null;
    try {
      const snapshot = await fastClient
        .get<PmResearchTaskStatusResponse>(
          `/agent/pm-research-tasks/${encodeURIComponent(taskId)}`,
        )
        .then((r) => r.data);
      const evt = pmTaskStatusToEvent(taskId, snapshot);
      if (isPmResearchTaskTerminalEvent(evt)) {
        return evt;
      }
    } catch {
      // best-effort recovery poll
    }
    await new Promise((resolve) => window.setTimeout(resolve, intervalMs));
  }
  return null;
}

export function streamPmResearchTask(
  taskId: string,
  handlers: {
    onEvent?: (event: PmResearchTaskEvent) => void;
    onNamedEvent?: (eventName: string, event: PmResearchTaskEvent) => void;
    onAnswerDelta?: (event: PmResearchTaskAnswerDeltaEvent) => void;
    onImageContextWarning?: (payload: { message: string; detail?: string; code?: string }) => void;
    onError?: (error: string) => void;
    onDone?: (event: PmResearchTaskEvent) => void;
  },
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let currentEvent = '';
  let currentData = '';
  let lastEvent: PmResearchTaskEvent | null = null;
  let doneEmitted = false;

  fetch(`${baseUrl}/api/v1/agent/pm-research-tasks/${encodeURIComponent(taskId)}/events`, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${token}`,
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
  }).then(async (response) => {
    if (!response.ok) {
      const text = await response.text();
      handlers.onError?.(`请求失败: ${response.status} ${text}`);
      return;
    }

    const stream = response.body;
    if (!stream) {
      handlers.onError?.('无响应体');
      return;
    }

    reader = stream.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    const flush = () => {
      if (!currentEvent || !currentData) {
        currentEvent = '';
        currentData = '';
        return;
      }
      if (aborted) {
        currentEvent = '';
        currentData = '';
        return;
      }
      try {
        const eventName = currentEvent || 'task_event';
        if (eventName === 'image_context_warning') {
          const payload = JSON.parse(currentData) as {
            message?: string;
            detail?: string;
            code?: string;
            imageContextWarning?: {
              message?: string;
              detail?: string;
              code?: string;
            };
          };
          const warning = payload.imageContextWarning ?? payload;
          handlers.onImageContextWarning?.({
            message: warning.message || '图片解析部分失败，系统将继续基于可用信息回答。',
            detail: warning.detail,
            code: warning.code,
          });
          currentEvent = '';
          currentData = '';
          return;
        }
        if (eventName === 'pm_answer_delta') {
          const payload = JSON.parse(currentData) as PmResearchTaskAnswerDeltaEvent;
          handlers.onAnswerDelta?.(payload);
          currentEvent = '';
          currentData = '';
          return;
        }
        const payload = JSON.parse(currentData) as PmResearchTaskEvent;
        if (
          eventName === 'task_event' ||
          eventName === 'subtask_started' ||
          eventName === 'subtask_completed' ||
          eventName === 'subtask_failed' ||
          eventName === 'merge_started' ||
          eventName === 'merge_completed'
        ) {
          lastEvent = payload;
          handlers.onNamedEvent?.(eventName, payload);
          handlers.onEvent?.(payload);
          if (isPmResearchTaskTerminalEvent(payload) && !doneEmitted) {
            doneEmitted = true;
            handlers.onDone?.(payload);
          }
        }
      } catch {
        // Ignore malformed SSE records and continue with later events.
      }
      currentEvent = '';
      currentData = '';
    };

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        flush();
        if (!aborted && !doneEmitted) {
          if (lastEvent && isPmResearchTaskTerminalEvent(lastEvent)) {
            doneEmitted = true;
            handlers.onDone?.(lastEvent);
          } else {
            const recovered = await pollPmTaskTerminalEvent(taskId, () => aborted);
            if (!aborted && recovered) {
              lastEvent = recovered;
              handlers.onEvent?.(recovered);
              doneEmitted = true;
              handlers.onDone?.(recovered);
            } else if (!aborted) {
              handlers.onError?.('任务事件流中断，且未获取到终态结果');
            }
          }
        }
        break;
      }

      const chunk = decoder.decode(value, { stream: true });
      buffer += chunk;

      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const raw of lines) {
        const trimmed = raw.trim();
        if (!trimmed) {
          flush();
          continue;
        }
        if (trimmed.startsWith('event:')) {
          flush();
          currentEvent = trimmed.slice(6).trim();
        } else if (trimmed.startsWith('data:')) {
          currentData = trimmed.slice(5).trim();
        } else if (currentData) {
          currentData += '\n' + trimmed;
        }
      }
    }
  }).catch((err) => {
    handlers.onError?.(err.message ?? 'stream error');
  });

  return () => {
    aborted = true;
    reader?.cancel();
  };
}
