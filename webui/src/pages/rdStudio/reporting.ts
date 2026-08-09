import type { RdFileChange, RdRepository, RdTask, RdTaskEvent, RdTestRun } from '@/types';
import { RD_FOLLOW_UP_CONTEXT_MAX_CHARS, RD_FOLLOW_UP_DIFF_MAX_CHARS } from './constants';
import type { RuntimeConfigSnapshot, RuntimeToolCall } from './types';
import { repoLabel } from './utils';

export function formatDurationMs(value?: number | null) {
  if (!value || value <= 0) return '-';
  if (value >= 60_000) return `${(value / 60_000).toFixed(1)}m`;
  if (value >= 1000) return `${(value / 1000).toFixed(1)}s`;
  return `${Math.round(value)}ms`;
}

function markdownFence(content?: string | null, language = '') {
  const safe = (content ?? '').trim() || '(empty)';
  return `~~~${language}\n${safe}\n~~~`;
}

function truncateMiddle(value: string, maxChars: number): string {
  if (value.length <= maxChars) return value;
  const head = Math.floor(maxChars * 0.6);
  const tail = Math.max(0, maxChars - head - 40);
  return `${value.slice(0, head)}\n...[middle truncated]...\n${value.slice(-tail)}`;
}

export function buildRdTaskReportMarkdown(params: {
  task: RdTask;
  repository?: RdRepository | null;
  changes: RdFileChange[];
  tests: RdTestRun[];
  events: RdTaskEvent[];
  runtimeToolCalls: RuntimeToolCall[];
  runtimeConfig: RuntimeConfigSnapshot;
}) {
  const { task, repository, changes, tests, events, runtimeToolCalls, runtimeConfig } = params;
  const lines: string[] = [
    `# ${task.title}`,
    '',
    `- Task ID: ${task.id}`,
    `- Status: ${task.status}`,
    `- Mode: ${task.mode}`,
    `- Model: ${task.model || '-'}`,
    `- Repository: ${repository ? repoLabel(repository) : '-'}`,
    `- Created At: ${task.createdAt}`,
    `- Completed At: ${task.completedAt || '-'}`,
    '',
    '## User Request',
    '',
    task.prompt,
  ];

  if (task.planMd?.trim()) lines.push('', '## Plan', '', task.planMd.trim());
  if (task.answerMd?.trim()) lines.push('', '## Final Summary', '', task.answerMd.trim());
  if (task.reviewMd?.trim()) lines.push('', '## Code Review', '', task.reviewMd.trim());
  if (task.errorMessage?.trim()) lines.push('', '## Error', '', task.errorMessage.trim());

  if (runtimeConfig.mcpServers.length > 0 || runtimeConfig.skills.length > 0 || runtimeConfig.permissionMode) {
    lines.push(
      '',
      '## Runtime Extensions',
      '',
      `- MCP Servers: ${runtimeConfig.mcpServers.join(', ') || '-'}`,
      `- Skills: ${runtimeConfig.skills.join(', ') || '-'}`,
      `- Permission Mode: ${runtimeConfig.permissionMode || '-'}`,
    );
  }

  if (changes.length > 0) {
    lines.push('', '## Diff', '');
    for (const change of changes) {
      lines.push(
        `### ${change.filePath}`,
        '',
        `- Change Type: ${change.changeType}`,
        `- Applied: ${change.applied ? 'yes' : 'no'}`,
        '',
        markdownFence(change.diffPatch, 'diff'),
        '',
      );
    }
  }

  if (tests.length > 0) {
    lines.push('', '## Test Runs', '');
    for (const test of tests.slice(0, 10)) {
      lines.push(
        `### ${test.command}`,
        '',
        `- Status: ${test.status}`,
        `- Exit Code: ${test.exitCode ?? '-'}`,
        `- Duration: ${formatDurationMs(test.durationMs)}`,
        '',
      );
      if (test.stdoutText?.trim()) lines.push('Stdout:', '', markdownFence(test.stdoutText, 'text'), '');
      if (test.stderrText?.trim()) lines.push('Stderr:', '', markdownFence(test.stderrText, 'text'), '');
    }
  }

  if (runtimeToolCalls.length > 0) {
    lines.push('', '## Runtime Tool Calls', '');
    for (const call of runtimeToolCalls.slice(0, 30)) {
      lines.push(
        `### ${call.toolName || 'Unknown Tool'} #${call.index ?? '-'}`,
        '',
        `- Source: ${call.sourceName ? `${call.source}:${call.sourceName}` : call.source || '-'}`,
        `- Error: ${call.isError ? 'yes' : 'no'}`,
        `- Duration: ${call.durationMs ?? 0}ms`,
        '',
      );
      if (call.input?.trim()) lines.push('Input:', '', markdownFence(call.input, 'json'), '');
      if (call.output?.trim()) lines.push('Output:', '', markdownFence(call.output, 'text'), '');
    }
  }

  if (events.length > 0) {
    lines.push('', '## Timeline', '');
    for (const event of events) {
      lines.push(`- ${event.createdAt} · ${event.stage} · ${event.status}${event.message ? ` · ${event.message}` : ''}`);
    }
  }

  if (task.prTitle || task.prDescription) {
    lines.push('', '## PR Output', '');
    if (task.prTitle) lines.push(`### ${task.prTitle}`, '');
    if (task.prDescription) lines.push(task.prDescription);
  }

  return lines.join('\n').trimEnd() + '\n';
}

export function buildRdFollowUpPrompt(params: {
  task: RdTask;
  changes: RdFileChange[];
  tests: RdTestRun[];
  userPrompt: string;
}) {
  const { task, changes, tests, userPrompt } = params;
  const contextLines: string[] = [
    '请基于上一轮研发任务继续处理。不要重复上一轮已经完成的解释；如果需要改代码，仍然只在 unifiedDiff 字段输出可审查 Diff，等待 AOS 人工确认后应用。',
    '',
    '# 上一轮任务',
    `- task_id: ${task.id}`,
    `- status: ${task.status}`,
    `- mode: ${task.mode}`,
    `- model: ${task.model || '-'}`,
    '',
    '## 上一轮用户需求',
    truncateMiddle(task.prompt.trim(), 2200),
  ];

  if (task.planMd?.trim()) {
    contextLines.push('', '## 上一轮计划', truncateMiddle(task.planMd.trim(), 2200));
  }
  if (task.answerMd?.trim()) {
    contextLines.push('', '## 上一轮最终总结', truncateMiddle(task.answerMd.trim(), 3000));
  }
  if (task.reviewMd?.trim()) {
    contextLines.push('', '## 上一轮代码审查', truncateMiddle(task.reviewMd.trim(), 2600));
  }
  if (task.errorMessage?.trim()) {
    contextLines.push('', '## 上一轮错误', truncateMiddle(task.errorMessage.trim(), 1600));
  }

  if (changes.length > 0) {
    contextLines.push('', '## 上一轮 Diff 摘要');
    for (const change of changes.slice(0, 8)) {
      contextLines.push(`- ${change.filePath} · ${change.changeType} · ${change.applied ? 'applied' : 'pending'}`);
    }
    const diffBudgetPerChange = Math.max(1200, Math.floor(RD_FOLLOW_UP_DIFF_MAX_CHARS / Math.min(changes.length, 4)));
    for (const change of changes.slice(0, 4)) {
      contextLines.push('', `### ${change.filePath}`, markdownFence(truncateMiddle(change.diffPatch, diffBudgetPerChange), 'diff'));
    }
  }

  if (tests.length > 0) {
    contextLines.push('', '## 最近测试结果');
    for (const test of tests.slice(0, 3)) {
      contextLines.push(
        `- ${test.status} · ${test.command} · exit=${test.exitCode ?? '-'} · ${formatDurationMs(test.durationMs)}`,
      );
      const output = [test.stdoutText, test.stderrText].filter(Boolean).join('\n').trim();
      if (output) contextLines.push(markdownFence(truncateMiddle(output, 1800), 'text'));
    }
  }

  const context = truncateMiddle(contextLines.join('\n'), RD_FOLLOW_UP_CONTEXT_MAX_CHARS);
  return `${context}\n\n# 本轮新需求\n${userPrompt.trim()}`;
}
