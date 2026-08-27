import { afterEach, describe, expect, it, vi } from 'vitest';
import { streamChatAdversarialRunEvents } from '@/api/agent';

function installStorage() {
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => {
      if (key === 'token') return 'test-token';
      if (key === 'tenant_id') return 'tenant-1';
      return null;
    },
  });
}

function okSse(body: string) {
  return new Response(body, {
    status: 200,
    headers: { 'Content-Type': 'text/event-stream' },
  });
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('chat adversarial event stream', () => {
  it('forwards model deltas in arrival order before the terminal event', async () => {
    installStorage();
    const fetchMock = vi.fn().mockResolvedValue(
      okSse([
        'id: 1',
        'event: adversarial_event',
        'data: {"seq":1,"runId":"run-1","phase":"initial","model":"model-a","messageId":"m-1","event":"model_started","status":"running"}',
        '',
        'id: 2',
        'event: adversarial_event',
        'data: {"seq":2,"runId":"run-1","phase":"initial","model":"model-a","messageId":"m-1","event":"model_delta","delta":"北京"}',
        '',
        'id: 3',
        'event: adversarial_event',
        'data: {"seq":3,"runId":"run-1","phase":"initial","model":"model-a","messageId":"m-1","event":"model_delta","delta":"天气"}',
        '',
        'id: 4',
        'event: adversarial_event',
        'data: {"seq":4,"runId":"run-1","phase":"system","messageId":"run-1","event":"run_completed","status":"completed"}',
        '',
      ].join('\n')),
    );
    vi.stubGlobal('fetch', fetchMock);

    const events: string[] = [];
    await new Promise<void>((resolve, reject) => {
      streamChatAdversarialRunEvents(
        'run-1',
        {
          onEvent: (event) => events.push(`${event.event}:${event.delta ?? ''}`),
          onEnd: resolve,
          onError: reject,
        },
        { afterSeq: 0 },
      );
    });

    expect(events).toEqual([
      'model_started:',
      'model_delta:北京',
      'model_delta:天气',
      'run_completed:',
    ]);
    expect(String(fetchMock.mock.calls[0]?.[0])).toContain(
      '/api/v1/agent/chat-adversarial-runs/run-1/events',
    );
  });

  it('passes an event cursor so reconnects do not replay the full run', async () => {
    installStorage();
    const fetchMock = vi.fn().mockResolvedValue(
      okSse(
        'event: adversarial_event\ndata: {"seq":8,"runId":"run-2","phase":"system","messageId":"run-2","event":"run_completed","status":"completed"}\n\n',
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    await new Promise<void>((resolve, reject) => {
      streamChatAdversarialRunEvents(
        'run-2',
        { onEnd: resolve, onError: reject },
        { afterSeq: 7 },
      );
    });

    expect(String(fetchMock.mock.calls[0]?.[0])).toContain('after_seq=7');
  });
});
