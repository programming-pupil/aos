import { describe, expect, it } from 'vitest';
import fc from 'fast-check';
import {
  computeZeroLossMeasurement,
  ZERO_LOSS_DEFAULT_THRESHOLD,
  type ZeroLossMeasurementInput,
} from '@/api/agent';

// Feature: super-assistant-hub, Property 9: Zero_Loss 召回率计算与达标判定
//
// For any probe result set and any configured threshold:
//   - recallRate MUST equal recalledCount / probeCount
//   - passed is true if and only if recallRate >= threshold (default 0.99)
//
// Validates: Requirements 4.5, 4.6
describe('Property 9: Zero_Loss 召回率计算与达标判定', () => {
  // Generator over raw (possibly messy) inputs. We intentionally include a
  // fractional/negative-friendly space so the property holds against the
  // sanitization the implementation performs (truncation + clamping).
  const rawInputArb = fc.record({
    sessionId: fc.string(),
    probeCount: fc.integer({ min: -50, max: 500 }),
    recalledCount: fc.integer({ min: -50, max: 500 }),
    // threshold optional: sometimes omitted (defaults to 0.99), sometimes a
    // value in [0, 1], and occasionally out of range to probe edge behavior.
    threshold: fc.option(fc.float({ min: 0, max: 1, noNaN: true }), {
      nil: undefined,
    }),
    removedMessages: fc.integer({ min: -10, max: 1000 }),
    summaryTokens: fc.integer({ min: -10, max: 100000 }),
  }) satisfies fc.Arbitrary<ZeroLossMeasurementInput>;

  it('recallRate equals recalledCount/probeCount and passed iff recallRate >= threshold', () => {
    fc.assert(
      fc.property(rawInputArb, (input) => {
        const result = computeZeroLossMeasurement(input);

        const expectedThreshold = input.threshold ?? ZERO_LOSS_DEFAULT_THRESHOLD;
        expect(result.threshold).toBe(expectedThreshold);

        // Sanitized counts: probeCount floored at 0, recalledCount clamped to
        // [0, probeCount]. These mirror the implementation contract so the
        // recallRate identity below is well-defined.
        expect(result.probeCount).toBeGreaterThanOrEqual(0);
        expect(result.recalledCount).toBeGreaterThanOrEqual(0);
        expect(result.recalledCount).toBeLessThanOrEqual(result.probeCount);

        // Core identity: recallRate === recalledCount / probeCount, with the
        // divide-by-zero guard (probeCount === 0 => rate 0).
        const expectedRate =
          result.probeCount === 0
            ? 0
            : result.recalledCount / result.probeCount;
        expect(result.recallRate).toBe(expectedRate);

        // recallRate is always a valid proportion in [0, 1].
        expect(result.recallRate).toBeGreaterThanOrEqual(0);
        expect(result.recallRate).toBeLessThanOrEqual(1);

        // passed is true iff recallRate >= threshold.
        expect(result.passed).toBe(result.recallRate >= result.threshold);

        return true;
      }),
      { numRuns: 100 },
    );
  });

  it('perfect recall passes at the default 0.99 threshold; a single miss below 100 probes fails', () => {
    fc.assert(
      fc.property(fc.integer({ min: 1, max: 500 }), (probeCount) => {
        const perfect = computeZeroLossMeasurement({
          sessionId: 's',
          probeCount,
          recalledCount: probeCount,
          removedMessages: 0,
          summaryTokens: 0,
        });
        expect(perfect.recallRate).toBe(1);
        expect(perfect.passed).toBe(true);

        // With fewer than 100 probes, missing exactly one drops recall below
        // 0.99, so it must not pass at the default threshold.
        if (probeCount < 100 && probeCount >= 1) {
          const oneMiss = computeZeroLossMeasurement({
            sessionId: 's',
            probeCount,
            recalledCount: probeCount - 1,
            removedMessages: 0,
            summaryTokens: 0,
          });
          expect(oneMiss.recallRate).toBeLessThan(ZERO_LOSS_DEFAULT_THRESHOLD);
          expect(oneMiss.passed).toBe(false);
        }
        return true;
      }),
      { numRuns: 100 },
    );
  });
});
