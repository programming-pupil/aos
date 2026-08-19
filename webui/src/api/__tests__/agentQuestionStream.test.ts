import { afterEach, describe, expect, it, vi } from 'vitest';
import { streamAgentSession, type RuntimeQuestionPaused } from '@/api/agent';

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

describe('durable runtime question stream', () => {
  it('treats question_paused as a clean terminal handoff', async () => {
    installStorage();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        okSse(
          [
            'event: question_paused',
            'data: {"sessionId":"session-question","runtimeTurnId":"turn-1","questions":[{"requestId":"question-1","turnId":"turn-1","invocationId":"tool-1","question":"Environment?","options":["staging","production"],"status":"pending","expiresAt":"2026-08-20T12:00:00Z","expired":false}]}',
            '',
          ].join('\n'),
        ),
      ),
    );

    const paused = await new Promise<RuntimeQuestionPaused>((resolve, reject) => {
      streamAgentSession('session-question', 'deploy', {
        onQuestionRequired: resolve,
        onError: reject,
      });
    });

    expect(paused.runtimeTurnId).toBe('turn-1');
    expect(paused.questions[0]).toEqual(
      expect.objectContaining({
        requestId: 'question-1',
        question: 'Environment?',
        options: ['staging', 'production'],
      }),
    );
  });

  it('submits every answer in one resume request', async () => {
    installStorage();
    const fetchMock = vi.fn().mockResolvedValue(
      okSse('event: stream_end\ndata: {"iterations":0}\n\n'),
    );
    vi.stubGlobal('fetch', fetchMock);

    await new Promise<void>((resolve, reject) => {
      streamAgentSession(
        'session-question',
        '',
        {
          onStreamEnd: () => resolve(),
          onError: reject,
        },
        {
          questionAnswers: [
            { requestId: 'question-1', answer: 'staging' },
            { requestId: 'question-2', answer: 'today' },
          ],
        },
      );
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({
      message: '',
      images: [],
      documents: [],
      turnOptions: {},
      questionAnswers: [
        { requestId: 'question-1', answer: 'staging' },
        { requestId: 'question-2', answer: 'today' },
      ],
    });
  });
});
