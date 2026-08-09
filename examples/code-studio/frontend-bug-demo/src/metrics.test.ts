import { describe, expect, it } from 'vitest';
import { calculateRoi, formatPercent } from './metrics';

describe('metrics', () => {
  it('calculates ROI for paid channels', () => {
    expect(calculateRoi(8554, 9100)).toBeCloseTo(0.94, 2);
  });

  it('keeps zero-cost channels from crashing the preview', () => {
    expect(calculateRoi(2195, 0)).toBe(0);
  });

  it('formats ROI as a percentage', () => {
    expect(formatPercent(0.94)).toBe('94.0%');
  });
});
