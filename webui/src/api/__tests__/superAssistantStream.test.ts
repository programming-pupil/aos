import { afterEach, describe, expect, it, vi } from 'vitest';
import { streamAgentSession } from '@/api/agent';

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

describe('Super Assistant durable stream handoff', () => {
  it('allocates and sends turnId before the first server event', async () => {
    installStorage();
    const fetchMock = vi.fn().mockResolvedValue(
      okSse(
        'id: 1\nevent: stream_end\ndata: {"iterations":0,"streamMode":"test"}\n\n',
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    const observedTurnIds: string[] = [];
    const finished = new Promise<void>((resolve, reject) => {
      streamAgentSession(
        'session-1',
        'hello',
        {
          onSuperAssistantTurnId: (turnId) => observedTurnIds.push(turnId),
          onStreamEnd: () => resolve(),
          onError: reject,
        },
        {
          images: [{
            url: '/api/v1/uploads/user/image.png',
            mediaType: 'image/png',
            name: 'image.png',
            fileId: 'image-1',
          }],
          documents: [{
            url: '/api/v1/uploads/user/report.xlsx',
            mediaType: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
            name: 'report.xlsx',
            fileId: 'doc-1',
          }],
          superAssistant: {
            app: 'chat',
            displayText: '/深度研究 hello',
            explicitCapability: 'pm_assistant',
          },
        },
      );
    });
    expect(observedTurnIds).toHaveLength(1);
    await finished;

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(String(init.body)) as Record<string, unknown>;
    expect(body.sessionId).toBe('session-1');
    expect(body.turnId).toEqual(expect.any(String));
    expect(body.text).toBe('hello');
    expect(body.displayText).toBe('/深度研究 hello');
    expect(body.explicitCapability).toBe('pm_assistant');
    expect(body.images).toEqual([
      expect.objectContaining({ fileId: 'image-1', name: 'image.png' }),
    ]);
    expect(body.documents).toEqual([
      expect.objectContaining({ fileId: 'doc-1', name: 'report.xlsx' }),
    ]);
    expect(String(body.turnId).length).toBeGreaterThan(10);
    expect(observedTurnIds[0]).toBe(body.turnId);
  });

  it('takes over through the persisted GET stream when POST SSE closes early', async () => {
    installStorage();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(okSse(''))
      .mockResolvedValueOnce(
        okSse(
          [
            'id: 2',
            'event: text',
            'data: {"text":"recovered answer"}',
            '',
            'id: 3',
            'event: stream_end',
            'data: {"iterations":1,"streamMode":"durable_turn_recovery"}',
            '',
          ].join('\n'),
        ),
      );
    vi.stubGlobal('fetch', fetchMock);

    let abort = () => {};
    const result = new Promise<string>((resolve, reject) => {
      abort = streamAgentSession(
        'session-2',
        'long task',
        {
          onStreamEnd: (_iterations, _usage, fullText) =>
            resolve(fullText ?? ''),
          onError: reject,
        },
        { superAssistant: { app: 'chat' } },
      );
    });
    const recovered = await result;
    abort();

    expect(recovered).toBe('recovered answer');
    expect(fetchMock).toHaveBeenCalledTimes(2);
    const [, firstInit] = fetchMock.mock.calls[0] as [string, RequestInit];
    const firstBody = JSON.parse(String(firstInit.body)) as { turnId: string };
    const secondUrl = String(fetchMock.mock.calls[1][0]);
    expect(secondUrl).toContain(
      `/super-assistant/turns/${encodeURIComponent(firstBody.turnId)}/events`,
    );
    expect(secondUrl).toContain('sessionId=session-2');
  });

  it('renders one final answer when unified and compatibility final events coexist', async () => {
    installStorage();
    const fetchMock = vi.fn().mockResolvedValue(
      okSse(
        [
          'id: 1',
          'event: final_delta',
          'data: {"text":"one answer"}',
          '',
          'id: 2',
          'event: text',
          'data: {"text":"one answer"}',
          '',
          'id: 3',
          'event: stream_end',
          'data: {"iterations":2,"streamMode":"unified_parent_agent"}',
          '',
        ].join('\n'),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);
    const streamed: string[] = [];
    const final = await new Promise<string>((resolve, reject) => {
      streamAgentSession(
        'session-3',
        'research',
        {
          onText: (text) => streamed.push(text),
          onStreamEnd: (_iterations, _usage, fullText) => resolve(fullText ?? ''),
          onError: reject,
        },
        { superAssistant: { app: 'chat' } },
      );
    });
    expect(final).toBe('one answer');
    expect(streamed.join('')).toBe('');
  });

  it('streams and reconstructs a direct parent answer from multiple final deltas', async () => {
    installStorage();
    const fetchMock = vi.fn().mockResolvedValue(
      okSse(
        [
          'id: 1',
          'event: final_delta',
          'data: {"index":0,"mode":"delta","text":"你好"}',
          '',
          'id: 2',
          'event: final_delta',
          'data: {"index":0,"mode":"delta","text":"，有什么可以帮你？"}',
          '',
          'id: 3',
          'event: stream_end',
          'data: {"iterations":1,"streamMode":"unified_parent_agent"}',
          '',
        ].join('\n'),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);
    const streamed: string[] = [];
    const final = await new Promise<string>((resolve, reject) => {
      streamAgentSession(
        'session-direct',
        'hello',
        {
          onText: (text) => streamed.push(text),
          onStreamEnd: (_iterations, _usage, fullText) => resolve(fullText ?? ''),
          onError: reject,
        },
        { superAssistant: { app: 'chat' } },
      );
    });
    expect(streamed.join('')).toBe('你好，有什么可以帮你？');
    expect(final).toBe('你好，有什么可以帮你？');
  });
});
