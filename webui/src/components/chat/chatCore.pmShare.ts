import type { ContentBlock } from '@/types';
import type { DisplayMessage } from './chatCore.types';

const PM_SHARE_PREVIEW_MAX_CHARS = 18000;

export interface PmSharePreviewPayload {
  schema: 'aos-pm-share-v1';
  title: string;
  generatedAt: string;
  messageId: string;
  taskId?: string | null;
  content: string;
  thinking?: string;
  truncated?: boolean;
}

function messageContentToPlain(content: string | ContentBlock[]): string {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return String(content ?? '');
  return content
    .map((block) => {
      if (block.type === 'text') return block.text;
      if (block.type === 'image') return '[image]';
      if (block.type === 'document') {
        return `[document:${block.name ?? 'Document'}]`;
      }
      return '[block]';
    })
    .join('\n');
}

function encodeUtf8ToBase64Url(raw: string): string {
  const bytes = new TextEncoder().encode(raw);
  let binary = '';
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/g, '');
}

export function buildPmSharePreviewPayload(
  msg: DisplayMessage,
): PmSharePreviewPayload | null {
  const contentRaw = messageContentToPlain(msg.content).trim();
  const thinkingRaw = (msg.thinking ?? '').trim();
  if (!contentRaw && !thinkingRaw) return null;
  const content = contentRaw.slice(0, PM_SHARE_PREVIEW_MAX_CHARS);
  const thinking = thinkingRaw.slice(0, PM_SHARE_PREVIEW_MAX_CHARS);
  return {
    schema: 'aos-pm-share-v1',
    title: 'AOS PM Reply Preview',
    generatedAt: new Date().toISOString(),
    messageId: msg.id,
    taskId: msg.pmTaskId ?? null,
    content,
    thinking: thinking.length > 0 ? thinking : undefined,
    truncated:
      contentRaw.length > PM_SHARE_PREVIEW_MAX_CHARS ||
      thinkingRaw.length > PM_SHARE_PREVIEW_MAX_CHARS,
  };
}

export function buildPmSharePreviewUrl(payload: PmSharePreviewPayload): string {
  const encoded = encodeURIComponent(
    encodeUtf8ToBase64Url(JSON.stringify(payload)),
  );
  const next = new URL(window.location.href);
  next.pathname = '/preview/share';
  next.search = `?d=${encoded}`;
  next.hash = '';
  return next.toString();
}
