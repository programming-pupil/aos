import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import {
  PmFinalDeliveryPanel,
  shouldShowPmFinalDelivery,
} from './PmFinalDeliveryPanel';

describe('PmFinalDeliveryPanel', () => {
  it('renders table highlights through the production Markdown component', () => {
    const table = [
      '| 指标 | 当前值 |',
      '|---|---:|',
      '| ROI | 1.23 |',
      '| AIPU | 17.11 |',
    ].join('\n');
    const t = ((_key: string, fallback?: string) => fallback ?? '') as TFunction;

    const html = renderToStaticMarkup(createElement(PmFinalDeliveryPanel, {
      t,
      title: '研究交付总结',
      highlights: [table],
      sources: [],
    }));

    expect(html).toContain('<table');
    expect(html).toContain('<th');
    expect(html).toContain('AIPU');
  });

  it('renders the persisted delivery body when reopening a completed session', () => {
    const t = ((_key: string, fallback?: string) => fallback ?? '') as TFunction;
    const html = renderToStaticMarkup(createElement(PmFinalDeliveryPanel, {
      t,
      title: '研究交付总结',
      highlights: [],
      sources: [],
      body: '# 完整交付结论\n\n历史正文仍然可见。',
    }));

    expect(html).toMatch(/<h1[^>]*>完整交付结论<\/h1>/);
    expect(html).toContain('历史正文仍然可见。');
  });

  it('shows a completed historical delivery without live stage state', () => {
    expect(shouldShowPmFinalDelivery({
      sessionSource: 'pm',
      executionUiEnabled: true,
      suppressExecutionUi: false,
      isStreaming: false,
      hasAssistantMessage: true,
      synthStatus: null,
      backgroundTaskStatus: null,
      latestTaskStatus: 'completed',
      body: '# 已持久化的研究结论\n\n历史会话正文。',
    })).toBe(true);
  });

  it('does not show a delivery card for a historical task that is still running', () => {
    expect(shouldShowPmFinalDelivery({
      sessionSource: 'pm',
      executionUiEnabled: true,
      suppressExecutionUi: false,
      isStreaming: false,
      hasAssistantMessage: true,
      synthStatus: null,
      backgroundTaskStatus: null,
      latestTaskStatus: 'running',
      body: '仍在研究中。',
    })).toBe(false);
  });
});
