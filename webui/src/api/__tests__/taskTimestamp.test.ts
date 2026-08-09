import { describe, expect, it } from 'vitest';

import { parseTaskTimestamp } from '../tasks';

describe('parseTaskTimestamp', () => {
  it('treats offset-free MySQL DATETIME values as UTC', () => {
    expect(parseTaskTimestamp('2026-07-27 08:41:16.849')).toBe(
      Date.UTC(2026, 6, 27, 8, 41, 16, 849),
    );
  });

  it('preserves explicit timezone offsets', () => {
    expect(parseTaskTimestamp('2026-07-27T16:41:16.849+08:00')).toBe(
      Date.UTC(2026, 6, 27, 8, 41, 16, 849),
    );
  });

  it('returns NaN for missing or malformed timestamps', () => {
    expect(Number.isNaN(parseTaskTimestamp(undefined))).toBe(true);
    expect(Number.isNaN(parseTaskTimestamp('not-a-date'))).toBe(true);
  });
});
