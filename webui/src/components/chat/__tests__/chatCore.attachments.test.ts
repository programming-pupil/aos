import { describe, expect, it } from 'vitest';
import { parsePersistedHistoryContent } from '../chatCore.attachments';

describe('persisted attachment history', () => {
  it('restores document and image blocks from durable JSON content', () => {
    const restored = parsePersistedHistoryContent(JSON.stringify([
      { type: 'text', text: '这个实验呢？' },
      {
        type: 'document',
        fileId: 'doc-1',
        media_type: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
        sourceType: 'url',
        data: '/api/v1/uploads/user/report.xlsx',
        name: 'report.xlsx',
      },
      {
        type: 'image',
        fileId: 'image-1',
        media_type: 'image/png',
        sourceType: 'url',
        data: '/api/v1/uploads/user/chart.png',
        name: 'chart.png',
        previewUrl: 'blob:https://aos.example/stale-preview',
      },
    ]));

    expect(Array.isArray(restored)).toBe(true);
    expect(restored).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'document', fileId: 'doc-1' }),
      expect.objectContaining({ type: 'image', fileId: 'image-1' }),
    ]));
    expect(Array.isArray(restored) ? restored.find((item) => item.type === 'image') : null).not.toHaveProperty('previewUrl');
  });

  it('does not reinterpret arbitrary JSON as message blocks', () => {
    const raw = '[{"name":"ordinary data"}]';
    expect(parsePersistedHistoryContent(raw)).toBe(raw);
  });
});
