import { describe, expect, it } from 'vitest';
import fc from 'fast-check';
import type { RouteDecisionEvent, ZeroLossMeasurement } from '@/api/agent';

// Feature: super-assistant-hub, Property 14: 序列化往返恒等
//
// For any trace event (RouteDecisionEvent) or memory/measurement record
// (ZeroLossMeasurement), serialize -> parse yields an equivalent object
// (round-trip identity). This is the frontend (fast-check) arm of Property 14;
// the Rust (proptest) arm covers RouteDecisionEvent / SessionCompactedEvent /
// AgentMemoryItem / MessageDto in `routes::super_assistant` and
// `routes::agent::agent_dtos`.
//
// Validates: Requirements 7.2
describe('Property 14: 序列化往返恒等', () => {
  // JSON-round-trippable doubles only: NaN / ±Infinity serialize to JSON
  // `null`, and `-0` serializes back as `0`, so they are excluded from the
  // generated space.
  const finiteDouble = fc
    .double({ noNaN: true, noDefaultInfinity: true })
    .filter((value) => !Object.is(value, -0));

  const routeDecisionEventArb: fc.Arbitrary<RouteDecisionEvent> = fc.record({
    event: fc.constant('route_decision' as const),
    targetCapability: fc.oneof(
      fc.constantFrom('ai_chat', 'pm_assistant', 'nl2sql', 'super_adversarial'),
      fc.string(),
    ),
    source: fc.constantFrom('explicit_override', 'llm_intent', 'rule_fallback'),
    // `number | null`: explicit overrides carry null confidence.
    confidence: fc.option(finiteDouble, { nil: null }),
    threshold: finiteDouble,
    bypassThreshold: fc.boolean(),
    // Optional `reason?: string`: sometimes omitted (undefined), which
    // JSON.stringify drops on both sides so the round-trip stays consistent.
    reason: fc.option(fc.string(), { nil: undefined }),
    turnId: fc.string(),
    createdAt: fc.string(),
  });

  const zeroLossMeasurementArb: fc.Arbitrary<ZeroLossMeasurement> = fc.record({
    sessionId: fc.string(),
    probeCount: fc.integer(),
    recalledCount: fc.integer(),
    recallRate: finiteDouble,
    threshold: finiteDouble,
    passed: fc.boolean(),
    removedMessages: fc.integer(),
    summaryTokens: finiteDouble,
    measuredAt: fc.string(),
  });

  it('RouteDecisionEvent survives serialize -> parse round-trip', () => {
    fc.assert(
      fc.property(routeDecisionEventArb, (event) => {
        const json = JSON.stringify(event);
        const parsed = JSON.parse(json) as RouteDecisionEvent;
        // serialize -> parse yields an equivalent object.
        expect(parsed).toEqual(event);
        // serialize -> parse -> serialize is identity-preserving.
        expect(JSON.stringify(parsed)).toBe(json);
        return true;
      }),
      { numRuns: 100 },
    );
  });

  it('ZeroLossMeasurement survives serialize -> parse round-trip', () => {
    fc.assert(
      fc.property(zeroLossMeasurementArb, (measurement) => {
        const json = JSON.stringify(measurement);
        const parsed = JSON.parse(json) as ZeroLossMeasurement;
        expect(parsed).toEqual(measurement);
        expect(JSON.stringify(parsed)).toBe(json);
        return true;
      }),
      { numRuns: 100 },
    );
  });
});
