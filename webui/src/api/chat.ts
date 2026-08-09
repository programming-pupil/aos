import { client, fastClient } from './client';
import type {
  ChatMessage,
  ChatSessionInfo,
  SendMessageResponse,
} from '@/types';

export const chatApi = {
  sendMessage: (data: { session_id?: string; model?: string; messages: ChatMessage[] }) =>
    client.post<SendMessageResponse>('/chat/message', data).then((r) => r.data),

  listSessions: () =>
    fastClient.get<{ sessions: ChatSessionInfo[]; total: number }>('/chat/sessions').then((r) => r.data),

  getSession: (sessionId: string) =>
    fastClient.get<{ sessionId: string; userId: string; messages: ChatMessage[] }>(
      `/chat/sessions/${encodeURIComponent(sessionId)}`
    ).then((r) => r.data),

  deleteSession: (sessionId: string) =>
    fastClient.delete(`/chat/sessions/${encodeURIComponent(sessionId)}`).then((r) => r.data),
};

export function streamChat(
  data: { session_id?: string; model?: string; messages: ChatMessage[] },
  handlers: {
    onContentBlockStart?: (index: number, blockType: string) => void;
    onContentBlockDelta?: (index: number, text: string) => void;
    /** Emitted when a tool call starts (MCP or built-in). */
    onToolCallStart?: (index: number, name: string, input: string) => void;
    /** Accumulated tool input JSON delta (for streaming input display). */
    onToolInputDelta?: (index: number, partialJson: string) => void;
    /** Emitted when a tool call completes. Includes tool name and input from the runtime. */
    onToolResult?: (index: number, toolName: string, input: string, output: string, isError: boolean, durationMs?: number) => void;
    onMessageDelta?: (usage: Record<string, number>, stopReason?: string) => void;
    onStreamEnd?: (sessionId: string) => void;
    onError?: (error: string) => void;
  },
  onAbort?: () => void
) {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const baseUrl = (client.defaults.baseURL ?? '/api/v1').replace('/api/v1', '');

  let aborted = false;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;

  fetch(`${baseUrl}/api/v1/chat/stream`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
      ...(tenantId ? { 'X-Tenant-ID': tenantId } : {}),
    },
    body: JSON.stringify(data),
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

    while (true) {
      if (aborted) break;
      const { done, value } = await reader.read();
      if (done) {
        break;
      }

      // Normalize line endings (SSE uses \r\n) and split
      const rawChunk = decoder.decode(value, { stream: true }).replace(/\r\n/g, '\n').replace(/\r/g, '\n');
      buffer += rawChunk;

      // Process all complete lines (ending with \n)
      const lines = buffer.split('\n');
      // Last element may be incomplete — keep it in buffer
      buffer = lines.pop() ?? '';

      for (const line of lines) {
        const trimmed = line.trim();
        if (trimmed.startsWith('event:')) {
          continue;
        }
        if (!trimmed.startsWith('data:')) {
          continue;
        }
        const payload = trimmed.slice(5).trim();
        const hasSessionId = payload.includes('"sessionId"');
        const cleanPayload = payload.replace(/\uFEFF/g, '');
        if (!cleanPayload) {
          continue;
        }
        try {
          const data = JSON.parse(cleanPayload);
          const eventType = data.type ?? (hasSessionId ? 'stream_end' : 'unknown');
          if (data.type === 'content_block_start') {
            handlers.onContentBlockStart?.(data.index, data.blockType);
            // Emit tool call start when we see a tool_use block
            if (data.blockType === 'tool_use') {
              const toolName = data.toolName ?? data.name ?? `tool-${data.index}`;
              const toolArgs = data.input ?? data.toolInput ?? '{}';
              handlers.onToolCallStart?.(data.index, toolName, typeof toolArgs === 'string' ? toolArgs : JSON.stringify(toolArgs));
            }
          } else if (data.type === 'content_block_delta') {
            // Forward text deltas
            if (data.delta?.type === 'text_delta') {
              handlers.onContentBlockDelta?.(data.index, data.delta.text);
            }
            // Forward tool input deltas
            if (data.delta?.type === 'input_json_delta') {
              handlers.onToolInputDelta?.(data.index, data.delta.partial_json);
            }
          } else if (data.type === 'content_block_stop') {
            // Tool input is complete — block is done
          } else if (data.type === 'message_delta') {
            handlers.onMessageDelta?.(data.usage, data.stopReason);
          } else if (data.type === 'tool_call') {
            // Tool call result from agent session stream
            handlers.onToolCallStart?.(data.index, data.tool_name, data.input ?? '{}');
          } else if (data.type === 'tool_result') {
            handlers.onToolResult?.(
              data.id ?? data.index,
              data.tool_name ?? '',
              data.input ?? '',
              data.output ?? data.result ?? '',
              data.is_error ?? false,
              data.duration_ms,
            );
          } else if (eventType === 'stream_end') {
            handlers.onStreamEnd?.(data.sessionId);
            break; // processed — exit read loop
          } else if (data.type === 'error') {
            handlers.onError?.(data.error);
          }
        } catch {
          // Ignore malformed SSE records and continue with later events.
        }
      }
    }
  }).catch((err) => {
    if (!aborted) handlers.onError?.(err.message);
  });

  return () => {
    aborted = true;
    reader?.cancel();
    onAbort?.();
  };
}
