import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import {
  extractMarkdownTableBlocks,
  Markdown,
  normalizeBrokenMarkdownSyntax,
  normalizeDenseMarkdownTables,
  normalizeMarkdownForRendering,
} from './markdownRenderer';

describe('normalizeDenseMarkdownTables', () => {
  it('repairs model output with collapsed GFM table row boundaries', () => {
    const source = [
      '## 发布检查清单',
      '',
      '| # | 检查项 | 说明 ||---|---|---|| 1 | 版本确认 | 核对代号 || 2 | 负责人 | 完成签核 |',
    ].join('\n');

    expect(normalizeDenseMarkdownTables(source)).toContain(
      '| # | 检查项 | 说明 |\n|---|---|---|\n| 1 | 版本确认 | 核对代号 |\n| 2 | 负责人 | 完成签核 |',
    );
  });

  it('does not reinterpret a valid empty table cell as a row boundary', () => {
    const source = '| A | B | C |\n|---|---|---|\n| value | | tail |';
    expect(normalizeDenseMarkdownTables(source)).toBe(source);
  });

  it('repairs spaced row boundaries and standalone pipe lines from PM reports', () => {
    const source = [
      '## 实施计划',
      '',
      '|',
      '| 能力域 | 具体任务 | 验收标准 |',
      '|',
      '|---|---|---| | P0-1 | 权限隔离 | A 用户看不到 B 用户 | | P0-2 | 记忆恢复 | 刷新后继续执行 |',
    ].join('\n');

    expect(normalizeDenseMarkdownTables(source)).toContain([
      '| 能力域 | 具体任务 | 验收标准 |',
      '|---|---|---|',
      '| P0-1 | 权限隔离 | A 用户看不到 B 用户 |',
      '| P0-2 | 记忆恢复 | 刷新后继续执行 |',
    ].join('\n'));
  });

  it('removes a blank line between a table header and separator', () => {
    const source = '| A | B |\n\n|---|---|\n| x | y |';
    expect(normalizeDenseMarkdownTables(source)).toBe('| A | B |\n|---|---|\n| x | y |');
  });

  it('does not rewrite table-like examples inside fenced code blocks', () => {
    const source = '```md\n| A | B ||---|---|| x | y |\n```';
    expect(normalizeDenseMarkdownTables(source)).toBe(source);
  });

  it('extracts a repaired table as one renderable Markdown block', () => {
    const source = '结论\n\n| A | B | |---|---| | x | y | | m | n |';
    expect(extractMarkdownTableBlocks(source)).toEqual([
      '| A | B |\n|---|---|\n| x | y |\n| m | n |',
    ]);
  });

  it('separates a numbered section title glued to a dense table header', () => {
    const source = '1. 1 四类方案全景 | 方案 | 优点 | 风险 ||---|---|---|| A | 快 | 高 || B | 稳 | 低 |';

    expect(normalizeDenseMarkdownTables(source)).toBe([
      '1. 1 四类方案全景',
      '',
      '| 方案 | 优点 | 风险 |',
      '|---|---|---|',
      '| A | 快 | 高 |',
      '| B | 稳 | 低 |',
    ].join('\n'));
  });

  it('renders a dense capability matrix glued to its decimal section title', () => {
    const source = '1.1 综合能力矩阵 | 平台 | 任务恢复 | 长任务交互 | 手机控制 ||------|---------|----------|-------|| OpenAI | 支持 | 一般 | 不支持 || AOS | 支持 | 实时进度 | 支持 |';
    const normalized = normalizeDenseMarkdownTables(source);
    const html = renderToStaticMarkup(createElement(Markdown, { relaxed: true, children: source }));

    expect(normalized).toContain([
      '1.1 综合能力矩阵',
      '',
      '| 平台 | 任务恢复 | 长任务交互 | 手机控制 |',
      '|------|---------|----------|-------|',
      '| OpenAI | 支持 | 一般 | 不支持 |',
      '| AOS | 支持 | 实时进度 | 支持 |',
    ].join('\n'));
    expect(html).toContain('<table');
    expect(html).toContain('<th');
    expect(html).toContain('实时进度');
  });

  it('renders the persisted horizontal comparison format shown by research history', () => {
    const source = [
      '六、横向对比总表',
      '',
      '1. 1 任务恢复机制对比表 | 平台 | 恢复范式 | 持久化载体 | 恢复粒度 | 幂等支持 | 证据状态 ||---|---|---|---|---|---|',
      '| LangGraph | 图 + Checkpointer | 状态检查点存储 | 节点级 | 推荐外部部署等键 | 官方文档 ✅ |',
      '| Temporal | Durable Execution | 事件日志重放 | 步骤/事件级 | 原生强一致 | 官方教程 ✅ |',
    ].join('\n');
    const normalized = normalizeDenseMarkdownTables(source);
    const html = renderToStaticMarkup(createElement(Markdown, { relaxed: true, children: source }));

    expect(normalized).toContain([
      '1. 1 任务恢复机制对比表',
      '',
      '| 平台 | 恢复范式 | 持久化载体 | 恢复粒度 | 幂等支持 | 证据状态 |',
      '|---|---|---|---|---|---|',
      '| LangGraph | 图 + Checkpointer | 状态检查点存储 | 节点级 | 推荐外部部署等键 | 官方文档 ✅ |',
    ].join('\n'));
    expect(html).toContain('<table');
    expect(html).toContain('LangGraph');
  });

  it('separates a persisted Chinese section title from an unprefixed table header', () => {
    const source = [
      '六、横向对比总表',
      '',
      '1.1 任务恢复机制对比表 | 平台 | 恢复范式 | 持久化载体 | 长任务交互 | 移动端通知',
      '|---|---|---|---|---|---|',
      '| LangGraph | 图 + Checkpointer | 数据库 | 事件流 | 有 |',
      '| AOS | AgentOps 状态机 | SQLite | 实时 | 有 |',
    ].join('\n');
    const normalized = normalizeDenseMarkdownTables(source);
    const html = renderToStaticMarkup(createElement(Markdown, { relaxed: true, children: source }));

    expect(normalized).toContain('1.1 任务恢复机制对比表\n\n| 平台 | 恢复范式 | 持久化载体 | 长任务交互 | 移动端通知 |');
    expect(html).toContain('<table');
    expect(html).toContain('Checkpointer');
    expect(html).toContain('实时');
  });

  it('repairs a table header with only its leading pipe omitted', () => {
    const source = '方案 | 优点 | 风险 ||---|---|---|| A | 快 | 高 |';

    expect(normalizeDenseMarkdownTables(source)).toBe([
      '| 方案 | 优点 | 风险 |',
      '|---|---|---|',
      '| A | 快 | 高 |',
    ].join('\n'));
  });

  it('leaves ordinary numbered prose containing pipes unchanged', () => {
    const source = '1. 执行 `cat a | sort`，然后查看输出。';
    expect(normalizeDenseMarkdownTables(source)).toBe(source);
  });

  it('repairs a persisted PM table whose separator has one excess column', () => {
    const source = [
      '我已经把修正版写入到工作区。',
      '',
      '| 问题 | 修正内容 |',
      '|---|------|--------| | 1 | 已补充价值用户完整口径 | | 2 | 已增加量化验收条件 |',
    ].join('\n');

    expect(normalizeDenseMarkdownTables(source)).toContain([
      '| 问题 | 修正内容 |',
      '|---|------|',
      '| 1 | 已补充价值用户完整口径 |',
      '| 2 | 已增加量化验收条件 |',
    ].join('\n'));
  });

  it('renders the malformed persisted PM table through the production GFM renderer', () => {
    const source = '| 问题 | 修正内容 |\n|---|------|--------| | 1 | 定义不完整 | 已修正 |';
    const html = renderToStaticMarkup(createElement(Markdown, { relaxed: true, children: source }));

    expect(html).toContain('<table');
    expect(html).toContain('<th');
    expect(html).toContain('修正内容');
  });

  it('repairs strong table cells split across streamed lines before rebuilding rows', () => {
    const source = [
      '| 维度 | 判断 |',
      '|---|---|',
      '| **因此可以取消 owner 校验?',
      '** | **绝对不可以，这是范畴错误** || **用于多租户授权?',
      '** | **禁止，会导致跨租户数据泄漏** |',
    ].join('\n');

    expect(normalizeDenseMarkdownTables(source)).toContain([
      '| **因此可以取消 owner 校验?** | **绝对不可以，这是范畴错误** |',
      '| **用于多租户授权?** | **禁止，会导致跨租户数据泄漏** |',
    ].join('\n'));
    expect(normalizeMarkdownForRendering(source, true)).toBe([
      '| 维度 | 判断 |',
      '|---|---|',
      '| **因此可以取消 owner 校验?** | **绝对不可以，这是范畴错误** |',
      '| **用于多租户授权?** | **禁止，会导致跨租户数据泄漏** |',
    ].join('\n'));
    const html = renderToStaticMarkup(createElement(Markdown, { relaxed: true, children: source }));
    expect(html).toContain('<table');
    expect(html).not.toContain('**');
  });

  it('repairs model headings that omit the required whitespace', () => {
    const source = '#一级标题\n\n＃＃二级标题';
    expect(normalizeBrokenMarkdownSyntax(source)).toBe('# 一级标题\n\n## 二级标题');
    const html = renderToStaticMarkup(createElement(Markdown, { relaxed: true, children: source }));
    expect(html).toContain('<h1');
    expect(html).toContain('<h2');
  });

  it('does not swallow following prose when a table bold marker never closes', () => {
    const source = '| **未闭合单元格\n\n后续独立结论';
    expect(normalizeBrokenMarkdownSyntax(source)).toBe(source);
  });
});
