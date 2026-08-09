import { describe, expect, it } from "vitest";

import { visibleAdversarialText } from "./AdversarialAuditPanel";

describe("visibleAdversarialText", () => {
  it("hides completed evidence and consensus protocol payloads", () => {
    expect(
      visibleAdversarialText(
        'Useful answer\n<aos_evidence_request>{"needed":true}</aos_evidence_request>',
      ),
    ).toBe("Useful answer");
    expect(
      visibleAdversarialText(
        'Revised answer\n<aos_consensus_vote>{"acceptConsensus":true}</aos_consensus_vote>',
      ),
    ).toBe("Revised answer");
  });

  it("hides partial protocol markers while streaming", () => {
    expect(visibleAdversarialText("Useful answer\n<aos_evid")).toBe("Useful answer");
    expect(visibleAdversarialText("Revised answer\n<aos_cons")).toBe("Revised answer");
  });

  it("does not alter ordinary model text", () => {
    expect(visibleAdversarialText("A normal answer with <code> markup.")).toBe(
      "A normal answer with <code> markup.",
    );
  });
});
