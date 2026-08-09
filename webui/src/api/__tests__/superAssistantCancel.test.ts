import { beforeEach, describe, expect, it, vi } from 'vitest';

const { post } = vi.hoisted(() => ({ post: vi.fn() }));

vi.mock('@/api/client', () => ({
  client: { defaults: { baseURL: '/api/v1' } },
  fastClient: { post },
}));

import { agentApi } from '@/api/agent';

describe('Super Assistant durable cancellation', () => {
  beforeEach(() => {
    post.mockReset();
  });

  it('cancels the durable parent turn instead of the ordinary session stream', async () => {
    post.mockResolvedValue({
      data: {
        turnId: 'turn/with spaces',
        sessionId: 'session-1',
        status: 'cancelled',
        cancelled: true,
      },
    });

    const result = await agentApi.cancelSuperAssistantTurn('turn/with spaces');

    expect(post).toHaveBeenCalledOnce();
    expect(post).toHaveBeenCalledWith(
      '/super-assistant/turns/turn%2Fwith%20spaces/cancel',
      {},
    );
    expect(result.cancelled).toBe(true);
  });
});
