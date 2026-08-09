import { describe, expect, it } from 'vitest';
import {
  pastedTextFileName,
  pastedTextLooksLikeSql,
  shouldAttachPastedText,
} from '../chatCore.paste';

describe('long pasted text attachments', () => {
  it('keeps ordinary prompts inline', () => {
    expect(shouldAttachPastedText('请解释这段内容')).toBe(false);
  });

  it('attaches long character and line payloads', () => {
    expect(shouldAttachPastedText('a'.repeat(4_000))).toBe(true);
    expect(shouldAttachPastedText(Array.from({ length: 80 }, (_, index) => `${index}`).join('\n'))).toBe(true);
  });

  it('uses a sql extension for pasted SQL', () => {
    const sql = `-- report\nWITH daily AS (SELECT 1 AS value)\nSELECT * FROM daily`;
    expect(pastedTextLooksLikeSql(sql)).toBe(true);
    expect(pastedTextFileName(sql, new Date('2026-07-21T08:00:00Z'))).toBe(
      'pasted-2026-07-21T08-00-00-000Z.sql',
    );
  });
});
