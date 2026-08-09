import { describe, expect, it } from "vitest";
import {
  isChatAdversarialRunId,
  isNl2sqlAttributionTaskId,
  isPmResearchTaskId,
} from "../specialistTaskIds";

describe("specialist task ids", () => {
  it("never routes attribution or adversarial tasks through PM research APIs", () => {
    const attribution = "nl2sql-attribution-task-050829fd";
    const adversarial = "chat-adv-run-123";

    expect(isNl2sqlAttributionTaskId(attribution)).toBe(true);
    expect(isChatAdversarialRunId(adversarial)).toBe(true);
    expect(isPmResearchTaskId(attribution)).toBe(false);
    expect(isPmResearchTaskId(adversarial)).toBe(false);
  });

  it("accepts only non-empty PM research task ids", () => {
    expect(isPmResearchTaskId("pm-research-task-123")).toBe(true);
    expect(isPmResearchTaskId("pm-task-legacy")).toBe(false);
    expect(isPmResearchTaskId("nl2sql-agent-task-123")).toBe(false);
    expect(isPmResearchTaskId("")).toBe(false);
    expect(isPmResearchTaskId(null)).toBe(false);
  });
});
