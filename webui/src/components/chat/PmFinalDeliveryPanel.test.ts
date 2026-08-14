import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import {
  PmFinalDeliveryPanel,
  shouldShowPmFinalDelivery,
} from './PmFinalDeliveryPanel';
import { attachPmFinalDeliveryArtifacts } from './ChatCore';

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

  it('uses the durable delivery artifact even when transient stage state is absent', () => {
    expect(shouldShowPmFinalDelivery({
      sessionSource: 'pm',
      executionUiEnabled: true,
      suppressExecutionUi: false,
      isStreaming: false,
      hasAssistantMessage: true,
      synthStatus: null,
      backgroundTaskStatus: null,
      latestTaskStatus: null,
      deliveryArtifact: {
        schemaVersion: 'pm-final-delivery-v1',
        taskId: 'task-1',
        taskStatus: 'degraded',
        qualityStatus: 'degraded',
        deliveryStatus: 'persisted',
        response: { text: '# 可交付结论' },
        stages: [],
        contentHash: 'hash',
      },
      body: '# 可交付结论',
    })).toBe(true);
  });

  it('restores the report projection when a historical assistant has a task id', () => {
    const messages = [{
      id: 'assistant-1',
      role: 'assistant' as const,
      content: '交付正文',
      pmTaskId: 'task-1',
    }];
    const restored = attachPmFinalDeliveryArtifacts(messages, [{
      schemaVersion: 'pm-final-delivery-v1',
      taskId: 'task-1',
      taskStatus: 'completed',
      qualityStatus: 'passed',
      deliveryStatus: 'persisted',
      response: {
        text: '交付正文',
        pm_report: { report_json_v3: { title: '目录' } },
      },
      stages: [],
      contentHash: 'hash',
    }]);
    expect(restored[0].pmFinalDelivery?.taskId).toBe('task-1');
    expect(restored[0].pmReport?.reportJsonV3?.title).toBe('目录');
  });

  it('materializes a missing assistant row from the durable artifact', () => {
    const restored = attachPmFinalDeliveryArtifacts([], [{
      schemaVersion: 'pm-final-delivery-v1',
      taskId: 'task-crashed',
      taskStatus: 'degraded',
      qualityStatus: 'degraded',
      deliveryStatus: 'persisted',
      response: { text: '# 恢复的交付' },
      stages: [],
      contentHash: 'hash',
    }]);
    expect(restored).toHaveLength(1);
    expect(restored[0].content).toBe('# 恢复的交付');
    expect(restored[0].pmTaskId).toBe('task-crashed');
  });
});
