import type { PmTaskDocumentInput, PmTaskImageInput } from '@/api';
import type { ContentBlock, DocumentBlock, ImageBlock } from '@/types';

export const PM_MAX_IMAGE_ATTACHMENTS = 5;

function isPersistedContentBlock(value: unknown): value is ContentBlock {
  if (!value || typeof value !== 'object') return false;
  const block = value as Record<string, unknown>;
  if (block.type === 'text') return typeof block.text === 'string';
  if (block.type !== 'image' && block.type !== 'document') return false;
  return (
    typeof block.data === 'string' &&
    typeof block.media_type === 'string' &&
    (block.sourceType === 'url' || block.sourceType === 'base64')
  );
}

function restorePersistedContentBlock(block: ContentBlock): ContentBlock {
  if (block.type !== 'image') return block;
  const image = block as ImageBlock;
  if (!image.previewUrl) return image;
  // blob: URLs only belong to the browser process that created them. Keeping
  // one in durable history makes a refreshed session prefer a dead preview
  // over the authenticated upload URL.
  const { previewUrl: _discardedPreview, ...durableImage } = image;
  return durableImage;
}

export function parsePersistedHistoryContent(value: unknown): string | ContentBlock[] {
  if (Array.isArray(value) && value.every(isPersistedContentBlock)) {
    return value.map(restorePersistedContentBlock);
  }
  if (typeof value !== 'string') return String(value ?? '');
  const trimmed = value.trim();
  if (!trimmed.startsWith('[') || !trimmed.endsWith(']')) return value;
  try {
    const parsed: unknown = JSON.parse(trimmed);
    return Array.isArray(parsed) && parsed.every(isPersistedContentBlock)
      ? parsed.map(restorePersistedContentBlock)
      : value;
  } catch {
    return value;
  }
}

export function collectPmTaskAttachments(attachments: ContentBlock[]): {
  images: PmTaskImageInput[];
  documents: PmTaskDocumentInput[];
  hasUnsupportedImageSource: boolean;
  hasUnsupportedDocumentSource: boolean;
} {
  const images: PmTaskImageInput[] = [];
  const documents: PmTaskDocumentInput[] = [];
  let hasUnsupportedImageSource = false;
  let hasUnsupportedDocumentSource = false;
  for (const att of attachments) {
    if (att.type === 'image') {
      const image = att as ImageBlock;
      if (image.sourceType !== 'url' || !image.data) {
        hasUnsupportedImageSource = true;
        continue;
      }
      images.push({
        url: image.data,
        mediaType: image.media_type,
        name: image.name,
        sizeBytes: image.sizeBytes,
        fileId: image.fileId,
      });
    } else if (att.type === 'document') {
      const document = att as DocumentBlock;
      if (document.sourceType !== 'url' || !document.data) {
        hasUnsupportedDocumentSource = true;
        continue;
      }
      documents.push({
        url: document.data,
        mediaType: document.media_type,
        name: document.name,
        sizeBytes: document.sizeBytes,
        fileId: document.fileId,
      });
    }
  }
  return { images, documents, hasUnsupportedImageSource, hasUnsupportedDocumentSource };
}

export function collectStreamImages(attachments: ContentBlock[]): PmTaskImageInput[] {
  const images: PmTaskImageInput[] = [];
  for (const att of attachments) {
    if (att.type !== 'image') continue;
    const image = att as ImageBlock;
    if (image.sourceType !== 'url' || !image.data) continue;
    images.push({
      url: image.data,
      mediaType: image.media_type,
      name: image.name,
      sizeBytes: image.sizeBytes,
      fileId: image.fileId,
    });
  }
  return images;
}

export function collectStreamDocuments(attachments: ContentBlock[]): PmTaskDocumentInput[] {
  const documents: PmTaskDocumentInput[] = [];
  for (const att of attachments) {
    if (att.type !== 'document') continue;
    const document = att as DocumentBlock;
    if (document.sourceType !== 'url' || !document.data) continue;
    documents.push({
      url: document.data,
      mediaType: document.media_type,
      name: document.name,
      sizeBytes: document.sizeBytes,
      fileId: document.fileId,
    });
  }
  return documents;
}
