// ── Context_Status panel unit tests (Super_Assistant, task 12.4) ────────────────
//
// Verifies the Context_Status panel surfaces the four memory signals correctly
// (Req 4.8 / 8.5): context usage %, compaction count, compacted state and the
// count of remembered (Unified_Memory) items.
//
// The webui test runner uses the `node` environment (no jsdom / testing-library),
// so we render deterministically to static HTML via `react-dom/server` and drive
// react-query from a prefilled cache. i18n is mocked so `t(key, default, opts)`
// returns the interpolated default string — matching the component's fallbacks —
// without initializing a live i18n instance.
//
// _Requirements: 8.5 (memory state perceivable), 1.1 (unified shell surfaces status)_

import { describe, expect, it, vi } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { queryKeys } from '@/api/queryKeys';
import type { AgentContextStatus, AgentMemoryItem } from '@/api/agent';

// Deterministic i18n: return the provided default string with {{var}} filled in.
vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (_key: string, defaultValue?: string, opts?: Record<string, unknown>) => {
      let out = defaultValue ?? _key;
      if (opts) {
        for (const [k, v] of Object.entries(opts)) {
          out = out.replace(new RegExp(`{{\\s*${k}\\s*}}`, 'g'), String(v));
        }
      }
      return out;
    },
  }),
}));

// Keep the module graph inert: queryFns are never invoked during static render
// (effects don't run), and the cache below serves all data.
vi.mock('@/api/agent', () => ({
  agentApi: {
    getSessionContextStatus: vi.fn(),
    listUnifiedMemories: vi.fn(),
  },
}));

import { ContextStatusPanel } from '@/components/chat/ContextStatusPanel';

function makeStatus(partial: Partial<AgentContextStatus> = {}): AgentContextStatus {
  return {
    sessionId: 's1',
    model: 'gpt-4o',
    provider: 'openai',
    messageCount: 12,
    estimatedTokens: 42000,
    tokenEstimator: 'heuristic',
    contextWindow: 128000,
    effectiveContextLimit: 100000,
    autoCompactTokenLimit: 90000,
    contextUsagePercent: 42,
    tokensUntilCompaction: 48000,
    shouldCompact: false,
    unknownContextWindow: false,
    compactionCount: 0,
    lastCompactionSummary: null,
    lastCompactionRemovedMessages: 0,
    state: 'active',
    memoryState: {
      sessionId: 's1',
      useMemories: true,
      generateMemories: true,
      pollutionState: 'clean',
      pollutionReason: null,
      lastExternalContextAt: null,
    },
    ...partial,
  };
}

function makeMemoryItem(id: string): AgentMemoryItem {
  return {
    id,
    scope: 'session',
    app: 'pm',
    sessionId: 's1',
    memoryType: 'fact',
    content: `remembered ${id}`,
    sourceType: 'extracted',
    confidence: 0.9,
    pinned: false,
    enabled: true,
    metadata: null,
    embeddingModel: null,
    embeddingDimensions: null,
    createdAt: '2024-01-01T00:00:00Z',
    updatedAt: '2024-01-01T00:00:00Z',
  };
}

function newQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
}

/** Prefill the exact query keys the panel reads for the given session. */
function seedCache(
  qc: QueryClient,
  sessionId: string,
  status: AgentContextStatus | null | undefined,
  memoryCount: number,
  memoryApp: 'chat' | 'pm' = 'pm',
) {
  const detail = queryKeys.agentSessions.detail(sessionId);
  if (status !== undefined) {
    qc.setQueryData([...detail, 'context-status'], status);
  }
  qc.setQueryData([...detail, 'memory-count', memoryApp], {
    items: Array.from({ length: memoryCount }, (_v, i) => makeMemoryItem(`m${i}`)),
  });
}

function renderPanel(qc: QueryClient, sessionId: string | null): string {
  return renderToStaticMarkup(
    <QueryClientProvider client={qc}>
      <ContextStatusPanel sessionId={sessionId} sessionSource="pm" />
    </QueryClientProvider>,
  );
}

describe('ContextStatusPanel', () => {
  it('renders nothing when there is no active session', () => {
    const qc = newQueryClient();
    const html = renderPanel(qc, null);
    expect(html).toBe('');
  });

  it('renders usage %, remembered count and "not compacted" for a fresh session', () => {
    const qc = newQueryClient();
    seedCache(qc, 's1', makeStatus({ contextUsagePercent: 42, compactionCount: 0 }), 3);

    const html = renderPanel(qc, 's1');

    // Panel + all three signal tags are present.
    expect(html).toContain('context-status-panel');
    expect(html).toContain('context-usage-tag');
    expect(html).toContain('context-compaction-tag');
    expect(html).toContain('context-remembered-tag');

    // Usage % is rounded from the raw contextUsagePercent.
    expect(html).toContain('Context 42%');
    // compactionCount === 0 → not compacted.
    expect(html).toContain('Not compacted');
    // 3 Unified_Memory items → "3 remembered".
    expect(html).toContain('3 remembered');
  });

  it('rounds fractional usage and shows compaction count when compacted', () => {
    const qc = newQueryClient();
    seedCache(qc, 's1', makeStatus({ contextUsagePercent: 87.6, compactionCount: 2 }), 5);

    const html = renderPanel(qc, 's1');

    // 87.6 → Math.round → 88.
    expect(html).toContain('Context 88%');
    // compactionCount === 2 → compacted with count.
    expect(html).toContain('Compacted ×2');
    expect(html).toContain('5 remembered');
  });

  it('clamps out-of-range usage into 0..100', () => {
    const qc = newQueryClient();
    seedCache(qc, 's1', makeStatus({ contextUsagePercent: 250 }), 0);

    const html = renderPanel(qc, 's1');

    // 250 clamps to 100; also verifies the zero-remembered case renders.
    expect(html).toContain('Context 100%');
    expect(html).toContain('0 remembered');
  });

  it('shows the unavailable fallback when context status is missing', () => {
    const qc = newQueryClient();
    // Seed a completed-but-empty context-status response. A completely absent
    // cache entry is still the initial loading state.
    seedCache(qc, 's1', null, 1);

    const html = renderPanel(qc, 's1');

    expect(html).toContain('Memory status unavailable');
    // No signal tags render without a status payload.
    expect(html).not.toContain('context-usage-tag');
  });
});
