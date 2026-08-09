import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { PmFinalDeliveryPanel } from './PmFinalDeliveryPanel';

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
});
