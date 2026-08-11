import { describe, expect, it } from "vitest";
import { shouldShowLegacyPmQueue } from "../superAssistantExecutionUi";

describe("super assistant execution UI", () => {
  it("hides the legacy PM queue from the unified super assistant", () => {
    expect(shouldShowLegacyPmQueue("pm", true)).toBe(false);
  });

  it("keeps the queue in the standalone PM assistant", () => {
    expect(shouldShowLegacyPmQueue("pm", false)).toBe(true);
    expect(shouldShowLegacyPmQueue("chat", false)).toBe(false);
  });
});
