// ── Markdown Renderer ──────────────────────────────────────────────────────────
// Full-featured renderer built on react-markdown + react-syntax-highlighter.
// Handles: syntax-highlighted code blocks, math (KaTeX), tables, task lists,
// images, and all standard GFM markdown. Safe HTML via rehype-sanitize.

// KaTeX CSS must be loaded globally (index.css or App.tsx).
// Add to index.css if not present:
//   @import url('https://cdn.jsdelivr.net/npm/katex@0.16.9/dist/katex.min.css');

import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import type { ComponentPropsWithoutRef } from 'react';
import { memo, useCallback, useState } from 'react';
import { LazyCodeHighlighter } from '@/components/code/LazyCodeHighlighter';
import { useAuthenticatedUploadUrl } from './AuthenticatedUploadImage';

// ── Helpers ────────────────────────────────────────────────────────────────────

function cssVar(name: string) {
  return `var(${name})`;
}

// ── Code Block ────────────────────────────────────────────────────────────────

function CopyButton({ code }: { code: string }) {
  const [copied, setCopied] = useState(false);
  const copy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch { /* ignore */ }
  }, [code]);

  return (
    <button
      onClick={copy}
      title={copied ? 'Copied!' : 'Copy code'}
      style={{
        position: 'absolute', top: 8, right: 8,
        background: 'rgba(255,255,255,0.08)', border: '1px solid rgba(255,255,255,0.12)',
        borderRadius: 4, padding: '3px 8px', cursor: 'pointer', fontSize: 11,
        color: copied ? '#4ade80' : cssVar('--text-muted'),
        transition: 'all 0.15s', lineHeight: 1.4,
      }}
    >
      {copied ? 'Copied!' : 'Copy'}
    </button>
  );
}

interface CodeBlockProps {
  inline?: boolean;
  className?: string;
  children?: React.ReactNode;
}

function CodeBlock({ inline, className, children }: CodeBlockProps) {
  const match = /language-(\w+)/.exec(className ?? '');
  const language = match?.[1] ?? '';
  const code = String(children ?? '').replace(/\n$/, '');
  const showLineNumbers = code.split('\n').length > 3;

  if (inline) {
    return (
      <code style={{
        background: cssVar('--bg-interactive'),
        padding: '1px 5px', borderRadius: 4, fontSize: '0.88em',
        color: cssVar('--text-link'),
        fontFamily: "'JetBrains Mono', 'Fira Code', monospace",
      }}>
        {children}
      </code>
    );
  }

  return (
    <div style={{ position: 'relative', margin: '8px 0' }}>
      {language && (
        <span style={{
          position: 'absolute', top: 0, left: 12,
          fontSize: 11, color: cssVar('--text-muted'),
          background: 'rgba(124,58,237,0.15)', borderRadius: '0 0 4px 4px',
          padding: '1px 8px', zIndex: 1,
        }}>
          {language}
        </span>
      )}
      <LazyCodeHighlighter
        code={code}
        language={language || 'text'}
        showLineNumbers={showLineNumbers}
        style={{
          margin: 0,
          borderRadius: 8,
          fontSize: 13,
          background: '#1a1d23',
          border: `1px solid ${cssVar('--border-default')}`,
        }}
        lineNumberStyle={{
          minWidth: '2.5em',
          paddingRight: '1em',
          color: cssVar('--text-muted'),
          opacity: 0.5,
          userSelect: 'none',
        }}
        codeTagStyle={{
          fontFamily: "'JetBrains Mono', 'Fira Code', monospace",
        }}
        wrapLongLines={false}
      />
      <CopyButton code={code} />
    </div>
  );
}

// ── Table ────────────────────────────────────────────────────────────────────

function MarkdownTable({ children }: { children?: React.ReactNode }) {
  return (
    <div style={{ width: '100%', maxWidth: '100%', overflowX: 'auto', margin: '12px 0', borderRadius: 8, border: `1px solid ${cssVar('--border-default')}` }}>
      <table style={{
        width: 'max-content', minWidth: '100%', borderCollapse: 'collapse', fontSize: '0.95em',
        tableLayout: 'auto',
        background: cssVar('--bg-elevated'),
      }}>
        {children}
      </table>
    </div>
  );
}

// ── Link ──────────────────────────────────────────────────────────────────────

function MarkdownLink({ href, children }: ComponentPropsWithoutRef<'a'>) {
  const normalizedHref = (href ?? '').trim();
  const hrefForDetect = normalizedHref.split('#')[0]?.split('?')[0]?.toLowerCase() ?? '';
  const isAudio = /\.(mp3|wav|ogg|m4a|aac|flac|webm)$/.test(hrefForDetect);
  if (isAudio) {
    return (
      <div style={{ display: 'grid', gap: 6, margin: '6px 0' }}>
        <audio controls preload="none" src={normalizedHref} style={{ width: '100%' }} />
        <a
          href={normalizedHref}
          target="_blank"
          rel="noopener noreferrer"
          style={{
            color: cssVar('--text-link'),
            textDecoration: 'underline',
            textDecorationColor: 'rgba(96,165,250,0.4)',
            fontSize: 12,
            overflowWrap: 'anywhere',
            wordBreak: 'break-word',
          }}
        >
          {children}
        </a>
      </div>
    );
  }
  const isExternal = normalizedHref.startsWith('http://') || normalizedHref.startsWith('https://');
  return (
    <a
      href={normalizedHref}
      target={isExternal ? '_blank' : undefined}
      rel={isExternal ? 'noopener noreferrer' : undefined}
      style={{
        color: cssVar('--text-link'),
        textDecoration: 'underline',
        textDecorationColor: 'rgba(96,165,250,0.4)',
        overflowWrap: 'anywhere',
        wordBreak: 'break-word',
      }}
    >
      {children}
    </a>
  );
}

// ── Image ────────────────────────────────────────────────────────────────────

function MarkdownImage({ src, alt }: { src?: string; alt?: string }) {
  const [expanded, setExpanded] = useState(false);
  const resolvedSrc = useAuthenticatedUploadUrl(src);
  return (
    <>
      <img
        src={resolvedSrc}
        alt={alt ?? ''}
        onClick={() => setExpanded(true)}
        style={{
          maxWidth: '100%', borderRadius: 8, cursor: 'zoom-in',
          border: `1px solid ${cssVar('--border-default')}`,
          margin: '4px 0',
        }}
        loading="lazy"
      />
      {expanded && (
        <div
          style={{
            position: 'fixed', inset: 0, zIndex: 1000,
            background: 'rgba(0,0,0,0.85)',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            cursor: 'zoom-out',
          }}
          onClick={() => setExpanded(false)}
        >
          <img
            src={resolvedSrc}
            alt={alt ?? ''}
            style={{ maxWidth: '90vw', maxHeight: '90vh', objectFit: 'contain', borderRadius: 8 }}
          />
        </div>
      )}
    </>
  );
}

// ── Task List Item ────────────────────────────────────────────────────────────

function TaskListItem({ checked, children }: { checked?: boolean; children?: React.ReactNode }) {
  return (
    <li style={{ listStyle: 'none', marginLeft: '-20px', display: 'flex', alignItems: 'flex-start', gap: 8 }}>
      <input
        type="checkbox"
        checked={checked}
        readOnly
        style={{ marginTop: 4, accentColor: cssVar('--accent-ai'), flexShrink: 0 }}
      />
      <span style={{ textDecoration: checked ? 'line-through' : 'none', color: checked ? cssVar('--text-muted') : cssVar('--text-primary') }}>
        {children}
      </span>
    </li>
  );
}

// ── Blockquote ───────────────────────────────────────────────────────────────

function Blockquote({ children }: { children?: React.ReactNode }) {
  return (
    <blockquote style={{
      borderLeft: `3px solid ${cssVar('--accent-ai')}`,
      paddingLeft: 14, margin: '12px 0',
      color: cssVar('--text-secondary'),
      fontStyle: 'italic',
      lineHeight: 1.8,
    }}>
      {children}
    </blockquote>
  );
}

// ── Heading anchors ─────────────────────────────────────────────────────────

function Heading({ level, children }: { level: 1 | 2 | 3 | 4 | 5 | 6; children?: React.ReactNode }) {
  const Tag = `h${level}` as 'h1' | 'h2' | 'h3' | 'h4' | 'h5' | 'h6';
  const sizes: Record<number, string> = {
    1: '1.6em',
    2: '1.35em',
    3: '1.18em',
    4: '1.08em',
    5: '1em',
    6: '0.95em',
  };
  return (
    <Tag style={{
      color: cssVar('--text-primary'),
      fontSize: sizes[level],
      fontWeight: 600,
      margin: level === 1 ? '20px 0 10px' : '16px 0 8px',
      lineHeight: 1.45,
    }}>
      {children}
    </Tag>
  );
}

// ── Main Renderer ────────────────────────────────────────────────────────────

export interface MarkdownProps {
  children: string;
  /** When true, renders without the outer <p> wrapper for inline use. */
  inline?: boolean;
  /** Adds conservative paragraph breaks for dense model text. */
  relaxed?: boolean;
  /** Hides Markdown horizontal rules for user-entered prompts where separators are noise. */
  suppressHr?: boolean;
}

function splitDenseMarkdownLine(line: string): string {
  const trimmed = line.trim();
  if (trimmed.length < 240) return line;
  if (
    trimmed.startsWith('|') ||
    trimmed.startsWith('- ') ||
    trimmed.startsWith('* ') ||
    /^\d+\.\s/.test(trimmed)
  ) {
    return line;
  }

  return line.replace(/([。！？!?])\s+(?=[\u4e00-\u9fa5A-Za-z0-9`])/g, '$1\n\n');
}

export function normalizeBrokenMarkdownSyntax(source: string): string {
  const repairedLines: string[] = [];
  let pendingTableLine: string | null = null;
  let pendingTableLineCount = 0;
  let inFence = false;
  const strongMarkerCount = (value: string) => (value.match(/\*\*/g) ?? []).length;

  for (const rawLine of source.split('\n')) {
    if (rawLine.trimStart().startsWith('```')) {
      if (pendingTableLine !== null) {
        repairedLines.push(pendingTableLine);
        pendingTableLine = null;
        pendingTableLineCount = 0;
      }
      inFence = !inFence;
      repairedLines.push(rawLine);
      continue;
    }
    if (inFence) {
      repairedLines.push(rawLine);
      continue;
    }

    const headingRepaired = rawLine
      .replace(/^(\s{0,3})(#{1,6})([^\s#])/u, '$1$2 $3')
      .replace(/^(\s{0,3})(＃{1,6})([^\s＃])/u, (_match, indent, marks, text) =>
        `${indent}${'#'.repeat(String(marks).length)} ${text}`,
      );

    if (pendingTableLine !== null) {
      const continuation = headingRepaired.trim();
      const isTableContinuation = continuation.startsWith('**') || continuation.includes('|');
      if (isTableContinuation && pendingTableLineCount < 4) {
        pendingTableLine += continuation.startsWith('**') ? continuation : ` ${continuation}`;
        pendingTableLineCount += 1;
        if (strongMarkerCount(pendingTableLine) % 2 === 0) {
          repairedLines.push(pendingTableLine);
          pendingTableLine = null;
          pendingTableLineCount = 0;
        }
        continue;
      }
      repairedLines.push(pendingTableLine);
      pendingTableLine = null;
      pendingTableLineCount = 0;
    }

    if (headingRepaired.includes('|') && strongMarkerCount(headingRepaired) % 2 === 1) {
      pendingTableLine = headingRepaired.trimEnd();
      pendingTableLineCount = 1;
    } else {
      repairedLines.push(headingRepaired);
    }
  }

  if (pendingTableLine !== null) repairedLines.push(pendingTableLine);
  return repairedLines.join('\n');
}

export function normalizeDenseMarkdownTables(source: string): string {
  const repairedSource = normalizeBrokenMarkdownSyntax(source);
  if (!repairedSource.includes('|')) return repairedSource;
  const separatorPattern = /\|\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|/;

  const separatorColumnCount = (line: string): number | null => {
    const match = separatorPattern.exec(line);
    if (!match) return null;
    const cells = match[0].slice(1, -1).split('|').map((cell) => cell.trim());
    return cells.length >= 2 && cells.every((cell) => /^:?-{3,}:?$/.test(cell))
      ? cells.length
      : null;
  };

  const tableRowColumnCount = (line: string): number | null => {
    const trimmed = line.trim();
    if (!trimmed.startsWith('|') || !trimmed.endsWith('|')) return null;
    if (separatorColumnCount(trimmed) !== null) return null;
    const cells = trimmed
      .slice(1, -1)
      .split('|')
      .map((cell) => cell.trim());
    return cells.length >= 2 && cells.some(Boolean) ? cells.length : null;
  };

  const splitRows = (input: string, columnCount: number): string[] | null => {
    const trimmed = input.trim();
    if (!trimmed) return [];
    const segments = trimmed.split('|');
    const rows: string[] = [];
    let cells: string[] = [];

    for (let index = 0; index < segments.length; index += 1) {
      const cell = segments[index].trim();
      const isOuterBoundary = (index === 0 || index === segments.length - 1) && !cell;
      if (isOuterBoundary || (cells.length === 0 && !cell)) continue;
      cells.push(cell);
      if (cells.length === columnCount) {
        rows.push(`| ${cells.join(' | ')} |`);
        cells = [];
      }
    }

    return cells.some(Boolean) || rows.length === 0 ? null : rows;
  };

  const expandDenseLine = (line: string, previousRowColumnCount?: number): string[] => {
    const match = separatorPattern.exec(line);
    if (!match || match.index === undefined) return [line];
    const prefix = line.slice(0, match.index).trim();
    const suffix = line.slice(match.index + match[0].length);
    const separatorCount = separatorColumnCount(match[0]);
    if (!separatorCount) return [line];

    // Model streams occasionally add one extra separator cell while keeping
    // the header and all data rows at the intended width. A separator is only
    // syntax, so an adjacent concrete row is the stronger source of truth.
    const prefixRowCount = tableRowColumnCount(prefix);
    const adjacentCount = prefixRowCount ?? previousRowColumnCount;
    const candidateCounts = [
      ...(adjacentCount && adjacentCount !== separatorCount ? [adjacentCount] : []),
      separatorCount,
    ];
    let columnCount: number | null = null;
    let rows: string[] | null = null;
    for (const candidate of candidateCounts) {
      const candidateRows = splitRows(suffix, candidate);
      if (candidateRows !== null) {
        columnCount = candidate;
        rows = candidateRows;
        break;
      }
    }
    if (!columnCount || rows === null) return [line];

    const separatorCells = match[0]
      .slice(1, -1)
      .split('|')
      .map((cell) => cell.trim());
    const normalizedSeparator = separatorCells.length >= columnCount
      ? `|${separatorCells.slice(0, columnCount).join('|')}|`
      : `|${Array.from({ length: columnCount }, () => '---').join('|')}|`;

    const expanded: string[] = [];
    if (prefix) {
      const withoutTrailingPipe = prefix.endsWith('|')
        ? prefix.slice(0, -1)
        : prefix;
      const prefixCells = withoutTrailingPipe
        .split('|')
        .map((cell) => cell.trim());
      if (prefix.startsWith('|') && prefix.endsWith('|')) {
        expanded.push(prefix);
      } else if (prefix.endsWith('|') && prefixCells.length === columnCount) {
        // Some models omit only the header row's leading pipe.
        expanded.push(`| ${prefixCells.join(' | ')} |`);
      } else if (
        prefix.endsWith('|')
        && prefixCells.length === columnCount + 1
        && prefixCells[0]
      ) {
        // A frequent streamed form glues a numbered section title directly to
        // the table header: `1. Title | A | B ||---|---|...`.
        expanded.push(prefixCells[0], '', `| ${prefixCells.slice(1).join(' | ')} |`);
      } else {
        return [line];
      }
    }
    expanded.push(normalizedSeparator);
    expanded.push(...rows);
    return expanded;
  };

  const expanded: string[] = [];
  let inFence = false;
  for (const rawLine of repairedSource.split('\n')) {
    if (rawLine.trimStart().startsWith('```')) {
      inFence = !inFence;
      expanded.push(rawLine);
      continue;
    }
    if (inFence) {
      expanded.push(rawLine);
      continue;
    }
    const normalizedLine = rawLine.replace(/([：:])\s+(\|[^\n]*\|)/g, '$1\n\n$2');
    for (const line of normalizedLine.split('\n')) {
      const previousNonEmpty = [...expanded]
        .reverse()
        .find((candidate) => candidate.trim().length > 0);
      const previousRowColumnCount = previousNonEmpty
        ? (tableRowColumnCount(previousNonEmpty) ?? undefined)
        : undefined;
      expanded.push(...expandDenseLine(line, previousRowColumnCount));
    }
  }

  // A common persisted/streamed form omits the leading pipe on the header and
  // glues a numbered section title to it.  GFM accepts the row, but treats the
  // title as a data column; normalize it before the table-width pass so the
  // title remains prose and the header starts at the intended column.
  const headerRepaired: string[] = [];
  for (let index = 0; index < expanded.length; index += 1) {
    const line = expanded[index];
    const next = expanded[index + 1]?.trim() ?? '';
    if (line.includes('|') && separatorColumnCount(next) !== null) {
      const trimmed = line.trim();
      const cells = trimmed
        .replace(/^\|/, '')
        .replace(/\|$/, '')
        .split('|')
        .map((cell) => cell.trim());
      const title = cells[0] ?? '';
      const isNumberedTitle = /^(?:\d+(?:\.\d+)*[.)]?|[一二三四五六七八九十零]+[、.])\s*\S/.test(title);
      if (isNumberedTitle && cells.length >= 3) {
        const headerCells = cells.slice(1);
        const separatorCells = next
          .replace(/^\|/, '')
          .replace(/\|$/, '')
          .split('|')
          .map((cell) => cell.trim())
          .slice(0, headerCells.length);
        headerRepaired.push(
          title,
          '',
          `| ${headerCells.join(' | ')} |`,
          `|${(separatorCells.length === headerCells.length
            ? separatorCells
            : Array.from({ length: headerCells.length }, () => '---')).join('|')}|`,
        );
        index += 1;
        continue;
      }
      if (!trimmed.startsWith('|') && cells.length >= 2) {
        const separatorCells = next
          .replace(/^\|/, '')
          .replace(/\|$/, '')
          .split('|')
          .map((cell) => cell.trim())
          .slice(0, cells.length);
        headerRepaired.push(
          `| ${cells.join(' | ')} |`,
          `|${(separatorCells.length === cells.length
            ? separatorCells
            : Array.from({ length: cells.length }, () => '---')).join('|')}|`,
        );
        index += 1;
        continue;
      }
    }
    headerRepaired.push(line);
  }

  const rowsExpanded: string[] = [];
  let activeTableColumnCount: number | null = null;
  for (const line of headerRepaired) {
    const trimmed = line.trim();
    const separatorCount = separatorColumnCount(trimmed);
    if (separatorCount !== null) {
      activeTableColumnCount = separatorCount;
      rowsExpanded.push(line);
      continue;
    }
    if (!trimmed) {
      rowsExpanded.push(line);
      continue;
    }
    if (activeTableColumnCount !== null && trimmed.includes('|')) {
      const rows = splitRows(trimmed, activeTableColumnCount);
      if (rows !== null) {
        rowsExpanded.push(...(rows.length > 1 ? rows : [line]));
        continue;
      }
    }
    activeTableColumnCount = null;
    rowsExpanded.push(line);
  }
  expanded.splice(0, expanded.length, ...rowsExpanded);

  const nearestNonEmpty = (start: number, direction: -1 | 1): string | null => {
    for (let index = start + direction; index >= 0 && index < expanded.length; index += direction) {
      const candidate = expanded[index].trim();
      if (candidate) return candidate;
    }
    return null;
  };

  const hasNearbySeparator = (start: number, direction: -1 | 1): boolean => {
    let tableLinesSeen = 0;
    for (let index = start + direction; index >= 0 && index < expanded.length; index += direction) {
      const candidate = expanded[index].trim();
      if (!candidate || candidate === '|') continue;
      if (!candidate.startsWith('|')) return false;
      if (separatorColumnCount(candidate) !== null) return true;
      tableLinesSeen += 1;
      if (tableLinesSeen >= 3) return false;
    }
    return false;
  };

  return expanded
    .filter((line, index) => {
      const trimmed = line.trim();
      if (trimmed !== '' && trimmed !== '|') return true;
      const previous = nearestNonEmpty(index, -1);
      const next = nearestNonEmpty(index, 1);
      const nextToTable = trimmed === '|'
        ? previous?.startsWith('|') || next?.startsWith('|')
        : previous?.startsWith('|') && next?.startsWith('|');
      const belongsToTable = nextToTable
        && (hasNearbySeparator(index, -1) || hasNearbySeparator(index, 1));
      return !belongsToTable;
    })
    .join('\n');
}

export function extractMarkdownTableBlocks(
  source: string,
  maxTables = 2,
  maxDataRows = 8,
): string[] {
  if (!source.includes('|') || maxTables <= 0) return [];
  const lines = normalizeDenseMarkdownTables(source).split('\n');
  const separatorRow = /^\|\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|$/;
  const tables: string[] = [];

  for (let index = 0; index + 1 < lines.length && tables.length < maxTables; index += 1) {
    const header = lines[index].trim();
    const separator = lines[index + 1].trim();
    if (!header.startsWith('|') || !header.endsWith('|') || !separatorRow.test(separator)) {
      continue;
    }
    const table = [header, separator];
    let cursor = index + 2;
    while (cursor < lines.length && table.length < maxDataRows + 2) {
      const row = lines[cursor].trim();
      if (!row.startsWith('|') || !row.endsWith('|')) break;
      table.push(row);
      cursor += 1;
    }
    tables.push(table.join('\n'));
    index = cursor - 1;
  }

  return tables;
}

const METRIC_TOKEN_PATTERN =
  '(?:ARPPU|ARPU|ROAS|ROI|AIPU|eCPM|CPM|CPC|CPA|CPI|CTR|CVR|LTV|CAC|DAU|WAU|MAU|UV|PV|GMV|MRR|ARR|NPS|SLA|SLO)';

function relaxMetricGlue(line: string): string {
  if (!line || /https?:\/\//i.test(line)) return line;
  let out = line.replace(new RegExp(`([^\\s])(${METRIC_TOKEN_PATTERN})(?=[\\d$¥￥%<>=+\\-]|[\\u3400-\\u9fff]|$)`, 'g'), '$1 $2');
  out = out.replace(new RegExp(`(${METRIC_TOKEN_PATTERN})(?=[\\d$¥￥%<>=+\\-]|[\\u3400-\\u9fff])`, 'g'), '$1 ');
  out = out.replace(/(\d,\d{3})(\d{1,3}(?:\.\d+)?%)/g, '$1 $2');
  out = out.replace(/(%)([$¥￥]?\d)/g, '$1 $2');
  out = out.replace(/(?:^|\s)[+＋]\s*\d+\s+more\.?/gi, '');
  return out.replace(/[ \t]{2,}/g, ' ').trimEnd();
}

function relaxPlainNumberedHeading(line: string): string {
  const trimmed = line.trim();
  if (
    /^#{1,6}\s+/.test(trimmed) ||
    trimmed.startsWith('|') ||
    /^[-*•]\s+/.test(trimmed) ||
    /^\d+\.\s/.test(trimmed)
  ) {
    return line;
  }
  const match = trimmed.match(/^([一二三四五六七八九十零]{1,4}|[0-9]{1,2})[、．.]\s*([\s\S]+)$/);
  if (!match) return line;
  const splitMatch = trimmed.match(new RegExp(`^((?:[一二三四五六七八九十零]{1,4}|[0-9]{1,2})[、．.]\\s*.{2,42}?)(?:\\s+|(?=${METRIC_TOKEN_PATTERN}|日均|指标|收入|成本|用户类型|人群|分层))(.*)$`, 'i'));
  if (splitMatch?.[2]?.trim()) {
    const body = splitMatch[2].trim();
    const bodyMetricHits = (body.match(new RegExp(METRIC_TOKEN_PATTERN, 'gi')) ?? []).length;
    const bodyTabularHits = (body.match(/(?:日均|收入|成本|占比|结论|用户|指标|ROI|ROAS|AIPU|eCPM|UV)/gi) ?? []).length;
    if (bodyMetricHits > 0 || bodyTabularHits >= 2) {
      return `### ${splitMatch[1].trim()}\n\n${body}`;
    }
  }
  const metricHits = (trimmed.match(new RegExp(METRIC_TOKEN_PATTERN, 'gi')) ?? []).length;
  const digitHits = (trimmed.match(/\d/g) ?? []).length;
  const tabularWordHits = (trimmed.match(/(?:日均|收入|成本|占比|分层|结论|用户|指标|ROI|ROAS|AIPU|eCPM|UV)/gi) ?? []).length;
  if (tabularWordHits >= 4 || (metricHits > 0 && digitHits >= 6)) {
    return line;
  }
  const looksDense = trimmed.length > 72 || (metricHits > 0 && digitHits >= 3);
  if (!looksDense && trimmed.length > 64) return line;

  if (trimmed.length <= 64) {
    return `### ${trimmed}`;
  }
  return line;
}

function relaxDenseMarkdown(source: string): string {
  let inFence = false;
  return normalizeDenseMarkdownTables(source)
    .replace(/([^\n])\s+(#{1,6}\s+)/g, '$1\n\n$2')
    .split('\n')
    .map((line) => {
      if (line.trimStart().startsWith('```')) {
        inFence = !inFence;
        return line;
      }
      if (inFence) return line;
      const metricRelaxed = relaxMetricGlue(line);
      if (metricRelaxed.trim().startsWith('|')) return metricRelaxed;
      const plainHeadingRelaxed = relaxPlainNumberedHeading(metricRelaxed);
      const withSectionBreaks = plainHeadingRelaxed
        .replace(
          /([^\n])\s+(?=(?:[一二三四五六七八九十]{1,3}|[0-9]{1,2})[、．.]\s*[\u3400-\u9fffA-Za-z])/g,
          '$1\n\n',
        )
        .replace(
          /([^\n])\s+(?=第[一二三四五六七八九十\d]{1,4}[章节部分步]\s*)/g,
          '$1\n\n',
        )
        .replace(
          /([。！？!?；;])\s*(?=(?:#{1,6}\s+|\*\*[^*\n]{2,72}\*\*))/g,
          '$1\n\n',
        );
      return splitDenseMarkdownLine(withSectionBreaks);
    })
    .join('\n');
}

const CJK_RE = /[\u3400-\u9fff\uf900-\ufaff]/;

function shouldKeepMathExpression(expression: string): boolean {
  const trimmed = expression.trim();
  if (!trimmed) return false;
  if (CJK_RE.test(trimmed)) return false;
  return /[\\^_{}=+\-*/<>|()[\]\d]/.test(trimmed);
}

function escapeMarkdownDollarMath(text: string): string {
  let output = '';
  let i = 0;
  let inFence = false;

  while (i < text.length) {
    if (text.startsWith('```', i)) {
      inFence = !inFence;
      output += '```';
      i += 3;
      continue;
    }

    if (inFence) {
      output += text[i];
      i += 1;
      continue;
    }

    if (text.startsWith('$$', i)) {
      const end = text.indexOf('$$', i + 2);
      if (end === -1) {
        output += '$$';
        i += 2;
        continue;
      }
      const inner = text.slice(i + 2, end);
      output += shouldKeepMathExpression(inner)
        ? `$$${inner}$$`
        : `\\$\\$${inner}\\$\\$`;
      i = end + 2;
      continue;
    }

    if (text[i] === '$' && text[i - 1] !== '\\' && text[i + 1] !== '$') {
      const end = text.indexOf('$', i + 1);
      if (end === -1) {
        output += text[i];
        i += 1;
        continue;
      }
      const inner = text.slice(i + 1, end);
      output += shouldKeepMathExpression(inner)
        ? `$${inner}$`
        : `\\$${inner}\\$`;
      i = end + 1;
      continue;
    }

    output += text[i];
    i += 1;
  }

  return output;
}

export function normalizeMarkdownForRendering(source: string, relaxed = false): string {
  const tableNormalizedSource = normalizeDenseMarkdownTables(source);
  return escapeMarkdownDollarMath(
    relaxed ? relaxDenseMarkdown(tableNormalizedSource) : tableNormalizedSource,
  );
}

function MarkdownImpl({ children: source, inline, relaxed, suppressHr }: MarkdownProps) {
  if (!source) return null;
  const renderedSource = normalizeMarkdownForRendering(source, relaxed);

  return (
    <div
      className="markdown-body"
      style={{
        color: cssVar('--text-primary'),
        fontSize: 'inherit',
        lineHeight: 'inherit',
        letterSpacing: 0,
        whiteSpace: 'normal',
        wordBreak: 'break-word',
        overflowWrap: 'break-word',
        textWrap: 'pretty',
      }}
    >
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[[rehypeKatex, { strict: 'ignore' }]]}
        components={{
          code({ className, children, ...props }) {
            const isInline = !(className?.startsWith('language-'));
            return (
              <CodeBlock
                inline={isInline}
                className={className}
                {...props}
              >
                {children}
              </CodeBlock>
            );
          },
          pre({ children }) {
            // The code component above handles everything; pre just passes through
            return <>{children}</>;
          },
          table({ children }) { return <MarkdownTable>{children}</MarkdownTable>; },
          th({ children }) {
            return (
              <th style={{
                padding: '8px 12px', textAlign: 'left',
                background: cssVar('--bg-interactive'),
                fontWeight: 600, fontSize: 12,
                borderBottom: `1px solid ${cssVar('--border-default')}`,
                color: cssVar('--text-secondary'),
                whiteSpace: 'normal', overflowWrap: 'normal', wordBreak: 'normal', minWidth: 96,
              }}>
                {children}
              </th>
            );
          },
          td({ children }) {
            return (
              <td style={{
                padding: '8px 12px',
                borderBottom: `1px solid ${cssVar('--border-subtle')}`,
                color: cssVar('--text-primary'),
                whiteSpace: 'normal', overflowWrap: 'normal', wordBreak: 'normal', minWidth: 96,
              }}>
                {children}
              </td>
            );
          },
          tr({ children }) {
            return <tr style={{ borderBottom: `1px solid ${cssVar('--border-subtle')}` }}>{children}</tr>;
          },
          a({ href, children }) { return <MarkdownLink href={href}>{children}</MarkdownLink>; },
          img({ src, alt }) { return <MarkdownImage src={src} alt={alt} />; },
          blockquote({ children }) { return <Blockquote>{children}</Blockquote>; },
          li({ children, ...props }) {
            // Task list item detection via data-checked attribute (injected by remark-gfm)
            const isTask = (props as Record<string, unknown>)['dataChecked'] !== undefined;
            if (isTask) {
              const checked = (props as Record<string, unknown>)['dataChecked'] !== 'false';
              return <TaskListItem checked={checked}>{children}</TaskListItem>;
            }
            return <li style={{ marginBottom: 6, lineHeight: 1.8 }}>{children}</li>;
          },
          ul({ children }) {
            return (
              <ul style={{ margin: '10px 0 12px', paddingLeft: 24 }}>
                {children}
              </ul>
            );
          },
          ol({ children }) {
            return (
              <ol style={{ margin: '10px 0 12px', paddingLeft: 24 }}>
                {children}
              </ol>
            );
          },
          h1({ children }) { return <Heading level={1}>{children}</Heading>; },
          h2({ children }) { return <Heading level={2}>{children}</Heading>; },
          h3({ children }) { return <Heading level={3}>{children}</Heading>; },
          h4({ children }) { return <Heading level={4}>{children}</Heading>; },
          h5({ children }) { return <Heading level={5}>{children}</Heading>; },
          h6({ children }) { return <Heading level={6}>{children}</Heading>; },
          p({ children }) {
            if (inline) return <span>{children}</span>;
            return <p style={{ margin: '10px 0', lineHeight: 1.85 }}>{children}</p>;
          },
          input({ ...props }) {
            // hide raw checkbox inputs — handled by li
            if ((props as Record<string, unknown>).type === 'checkbox') return null;
            return <input {...props} />;
          },
          del({ children }) {
            return <span style={{ textDecoration: 'none' }}>{children}</span>;
          },
          hr() {
            if (suppressHr) return null;
            return <hr style={{ border: 'none', borderTop: `1px solid ${cssVar('--border-default')}`, margin: '18px 0' }} />;
          },
        }}
      >
        {renderedSource}
      </ReactMarkdown>
    </div>
  );
}

// Historical bubbles and side panels can re-render while a different message
// streams. Keep the expensive markdown parse isolated when the source itself
// did not change.
export const Markdown = memo(MarkdownImpl);
