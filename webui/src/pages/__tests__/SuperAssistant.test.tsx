// ── Super_Assistant unified shell tests (task 12.4) ─────────────────────────────
//
// Verifies the merged single-session shell (Req 1.1 / 1.6 / 8.x):
//   - the former AI Chat / Ops Assistant / Data Attribution entries collapse into
//     ONE entry point driven by a single input box (no sub-scenario picker),
//   - the shell mounts on the unified `pm` deep-dialogue ChatCore variant,
//   - the Context_Status panel + unified-entry affordances are wired into the
//     top bar, and the input hint bar advertises chat / SQL / attachments.
//
// ChatCore is a heavy shared component; we stub it so the test stays fast and
// deterministic and we can assert exactly which props the shell passes. Rendering
// uses `react-dom/server` (the webui test env is `node`, no jsdom), and i18n is
// mocked to return the default strings.
//
// _Requirements: 1.1, 1.6, 8.2, 8.3, 8.4, 8.5_

import { describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

// Deterministic i18n: return the default string (no interpolation needed here).
vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (_key: string, defaultValue?: string) => defaultValue ?? _key,
  }),
}));

// Stub ChatCore: render the props the shell wires up so we can assert on the
// single input box, the session source, and the top-bar / hint-bar slots.
interface StubChatCoreProps {
  sessionSource?: string;
  inputPlaceholder?: string;
  topBarExtra?: ReactNode;
  inputHintBar?: ReactNode;
  inputToolbarExtra?: ReactNode;
  showMemoryButton?: boolean;
}

vi.mock('@/components/chat/ChatCore', () => ({
  ChatCore: (props: StubChatCoreProps) => (
    <div data-testid="chat-core">
      <span data-testid="session-source">{props.sessionSource}</span>
      <div data-testid="top-bar-extra">{props.topBarExtra}</div>
      <div data-testid="input-hint-bar">{props.inputHintBar}</div>
      <div data-testid="input-toolbar-extra">{props.inputToolbarExtra}</div>
      <span data-testid="show-memory-button">{String(props.showMemoryButton)}</span>
      <input data-testid="unified-input" placeholder={props.inputPlaceholder} />
    </div>
  ),
}));

import SuperAssistant from '@/pages/SuperAssistant';

function render(): string {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  return renderToStaticMarkup(
    <QueryClientProvider client={qc}>
      <SuperAssistant />
    </QueryClientProvider>,
  );
}

describe('SuperAssistant unified shell', () => {
  it('mounts a single unified input box (no sub-scenario picker)', () => {
    const html = render();

    // Exactly one input box — the single unified entry.
    const inputCount = (html.match(/<input/g) ?? []).length;
    expect(inputCount).toBe(1);

    // No <select> element → no sub-scenario / capability picker in the shell.
    expect(html).not.toContain('<select');

    // The placeholder makes the single box accept text/code/SQL/attachments.
    expect(html).toContain('Ask anything');
    expect(html).toContain('attachment');
  });

  it('uses the unified pm deep-dialogue ChatCore variant', () => {
    const html = render();
    expect(html).toContain('data-testid="chat-core"');
    expect(html).toContain('>pm<');
  });

  it('does not render the legacy data-attribution switch', () => {
    const html = render();
    expect(html).not.toContain('role="switch"');
    expect(html).not.toContain('dataAttributionSwitch');
  });

  it('hides the manual memory-management button', () => {
    const html = render();
    expect(html).toContain('data-testid="show-memory-button">false<');
  });

  it('surfaces the unified-entry / auto-routing / memory affordances in the top bar', () => {
    const html = render();
    // Menu merge → a single "Unified entry" surface with auto intent routing.
    expect(html).toContain('Unified entry');
    expect(html).toContain('Auto intent routing');
    expect(html).toContain('Context memory');
  });

  it('advertises chat, SQL and attachment handling plus evidence in the hint bar', () => {
    const html = render();
    // Streaming/evidence + capability hints (Req 8.2/8.3/8.4).
    expect(html).toContain('code');
    expect(html).toContain('SQL');
    expect(html).toContain('attachment');
    expect(html).toContain('evidence');
  });
});
