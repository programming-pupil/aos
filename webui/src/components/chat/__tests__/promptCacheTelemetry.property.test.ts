import fc from 'fast-check';
import { describe, expect, it } from 'vitest';
import {
  computePromptCacheHitRate,
  type PromptCacheTelemetrySample,
} from '@/components/chat/promptCacheTelemetry';

describe('Prompt Cache telemetry', () => {
  it('computes cache hit rate from available non-degraded samples', () => {
    // Feature: codex-parity-gaps, Property 12: Cache_Hit_Rate 计算正确性
    const sampleArb: fc.Arbitrary<PromptCacheTelemetrySample> = fc.record({
      inputTokens: fc.option(fc.integer({ min: 0, max: 500_000 }), { nil: null }),
      cacheReadInputTokens: fc.option(fc.integer({ min: 0, max: 500_000 }), { nil: null }),
      unexpected: fc.option(fc.boolean(), { nil: false }),
      reason: fc.option(fc.string(), { nil: null }),
    });

    fc.assert(
      fc.property(fc.array(sampleArb, { maxLength: 200 }), (samples) => {
        const result = computePromptCacheHitRate(samples);
        let expectedRead = 0;
        let expectedTotal = 0;

        for (const sample of samples) {
          if (sample.unexpected) continue;
          if (typeof sample.inputTokens !== 'number' || !Number.isFinite(sample.inputTokens) || sample.inputTokens < 0) {
            continue;
          }
          const cache =
            typeof sample.cacheReadInputTokens === 'number' &&
            Number.isFinite(sample.cacheReadInputTokens) &&
            sample.cacheReadInputTokens >= 0
              ? sample.cacheReadInputTokens
              : 0;
          expectedTotal += sample.inputTokens;
          expectedRead += Math.min(cache, sample.inputTokens);
        }

        expect(result.totalInputTokens).toBe(expectedTotal);
        expect(result.cacheReadInputTokens).toBe(expectedRead);
        expect(result.cacheHitRate).toBe(expectedTotal > 0 ? expectedRead / expectedTotal : 0);
      }),
      { numRuns: 100 },
    );
  });
});
