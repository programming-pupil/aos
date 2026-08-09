import { distance } from 'fastest-levenshtein';
import type { ContentBlock } from '@/types';
import type { SlashCommandDef } from './types';
import type { DisplayMessage } from './chatCore.types';

export function contentToPlain(c: string | ContentBlock[]): string {
  if (typeof c === 'string') return c;
  return c.map((b) => ('text' in b ? b.text : `[${b.type}]`)).join('\n');
}

export function parseToolName(qualifiedName: string): {
  source: 'mcp' | 'builtin' | 'skill';
  sourceName: string;
  tool: string;
} {
  if (qualifiedName.startsWith('mcp__')) {
    const parts = qualifiedName.slice(4).split('__');
    return {
      source: 'mcp',
      sourceName: parts[0] ?? '',
      tool: parts.slice(1).join('__'),
    };
  }
  if (qualifiedName.startsWith('skill__')) {
    const parts = qualifiedName.slice(6).split('__');
    return {
      source: 'skill',
      sourceName: parts[0] ?? '',
      tool: parts.slice(1).join('__'),
    };
  }
  return { source: 'builtin', sourceName: '', tool: qualifiedName };
}

export function mergeToolInput(existing: string, incoming: string): string {
  if (!incoming) return existing;
  if (!existing) return incoming;

  const prev = existing.trim();
  const next = incoming.trim();
  if (!next) return existing;

  if (
    (next.startsWith('{') || next.startsWith('[')) &&
    next.length >= prev.length
  ) {
    return incoming;
  }

  if (incoming.length >= existing.length && incoming.startsWith(existing)) {
    return incoming;
  }

  if (existing.endsWith(incoming) || incoming.endsWith(existing)) {
    return existing.length >= incoming.length ? existing : incoming;
  }

  return existing + incoming;
}

export function filterSlashCommands(
  allCommands: SlashCommandDef[],
  filter: string,
): SlashCommandDef[] {
  const f = filter.toLowerCase().trim();
  if (!f) return allCommands;
  const scored = allCommands.map((cmd) => {
    const nameLower = cmd.name.toLowerCase();
    const descLower = cmd.description.toLowerCase();
    if (nameLower === f) return { cmd, dist: 0 };
    if (nameLower.startsWith(f)) return { cmd, dist: 1 };
    if (nameLower.includes(f)) return { cmd, dist: 2 };
    if (descLower.includes(f)) return { cmd, dist: 3 };
    const nameDist = distance(f, nameLower);
    const descDist = distance(f, descLower.slice(0, 30));
    const dist = Math.min(nameDist, descDist);
    return { cmd, dist };
  });
  return scored
    .filter(({ dist }) => dist <= 3)
    .sort((a, b) => a.dist - b.dist)
    .map(({ cmd }) => cmd);
}

export function buildMessageContent(
  input: string,
  attachments: ContentBlock[],
): string | ContentBlock[] {
  const trimmed = input.trim();
  const blocks: ContentBlock[] = [...attachments];
  if (trimmed) blocks.push({ type: 'text', text: trimmed });
  if (blocks.length === 0) return '';
  if (blocks.length === 1 && blocks[0].type === 'text') return trimmed;
  return blocks;
}

export function buildReplyPrefix(
  replyTo: string | null,
  displayMessages: DisplayMessage[],
): string {
  if (!replyTo) return '';
  const repliedMsg = displayMessages.find((m) => m.id === replyTo);
  if (!repliedMsg) return '';
  const original = contentToPlain(repliedMsg.content);
  const quoted = original
    .split('\n')
    .map((line) => `> ${line}`)
    .join('\n');
  return `${quoted}\n\n`;
}
