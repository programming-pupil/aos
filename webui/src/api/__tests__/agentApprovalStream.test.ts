import { afterEach, describe, expect, it, vi } from 'vitest';
import { streamAgentSession, type RuntimeApprovalPaused } from '@/api/agent';

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

describe('durable runtime approval stream', () => {
  it('treats approval_paused as a clean terminal handoff', async () => {
    installStorage();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        okSse(
          [
            'event: approval_required',
            'data: {"turnId":"turn-1","invocationId":"tool-1","toolName":"shell"}',
            '',
            'event: approval_paused',
            'data: {"sessionId":"session-approval","runtimeTurnId":"turn-1","approvals":[{"requestId":"approval-1","turnId":"turn-1","invocationId":"tool-1","toolName":"shell","currentMode":"workspace-write","requiredMode":"danger-full-access","status":"pending","expiresAt":"2026-08-15T12:00:00Z","expired":false}]}',
            '',
          ].join('\n'),
        ),
      ),
    );

    const paused = await new Promise<RuntimeApprovalPaused>((resolve, reject) => {
      streamAgentSession('session-approval', 'run command', {
        onApprovalRequired: resolve,
        onError: reject,
      });
    });

    expect(paused.runtimeTurnId).toBe('turn-1');
    expect(paused.approvals).toEqual([
      expect.objectContaining({
        requestId: 'approval-1',
        invocationId: 'tool-1',
        toolName: 'shell',
      }),
    ]);
    expect(paused.approvals[0]).not.toHaveProperty('input');
  });

  it.each(['approve', 'deny', 'cancel'] as const)(
    'sends a minimal %s resolution without tool input',
    async (decision) => {
      installStorage();
      const fetchMock = vi.fn().mockResolvedValue(
        okSse('event: stream_end\ndata: {"iterations":0}\n\n'),
      );
      vi.stubGlobal('fetch', fetchMock);

      await new Promise<void>((resolve, reject) => {
        streamAgentSession(
          'session-approval',
          '',
          {
            onStreamEnd: () => resolve(),
            onError: reject,
          },
          {
            approval: {
              requestId: 'approval-1',
              decision,
              reason: 'user decision',
            },
          },
        );
      });

      const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      expect(body).toEqual({
        message: '',
        images: [],
        documents: [],
        turnOptions: {},
        approval: {
          requestId: 'approval-1',
          decision,
          reason: 'user decision',
        },
      });
      expect(JSON.stringify(body)).not.toContain('tool input');
      expect(JSON.stringify(body)).not.toContain('command');
    },
  );
});
