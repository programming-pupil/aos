import { describe, expect, it } from "vitest";

import { shouldShowPmPostStreamNotice } from "../chatCore.pmTypes";

describe("post-stream execution notice", () => {
  it("does not render a second assistant bubble after a normal final answer", () => {
    expect(shouldShowPmPostStreamNotice(true, false)).toBe(false);
  });

  it("stays visible while a real background specialist task is running", () => {
    expect(shouldShowPmPostStreamNotice(true, true)).toBe(true);
  });
});
