import { describe, expect, it } from "vitest";

import type { DisplayMessage } from "./chatCore.types";
import { reconcilePmHistoryTerminalAssistant } from "./pmTerminalReconciler";

function assistant(content: string): DisplayMessage {
  return {
    id: "assistant-1",
    role: "assistant",
    content,
    timestamp: 1,
  };
}

describe("reconcilePmHistoryTerminalAssistant", () => {
  it("preserves a richer historical research answer while restoring delivery metadata", () => {
    const fullAnswer = "完整研究正文。".repeat(80);
    const report = {
      schemaVersion: "3",
      reportJsonV3: { title: "交付结论", executiveSummary: "摘要" },
    };
    const result = reconcilePmHistoryTerminalAssistant(
      [assistant(fullAnswer)],
      "较短的交付摘要",
      {
        taskId: "task-1",
        taskStatus: "completed",
        pmReport: report,
        preserveRicherContent: true,
      },
    );

    expect(result).toHaveLength(1);
    expect(result[0].content).toBe(fullAnswer);
    expect(result[0].pmTaskId).toBe("task-1");
    expect(result[0].pmTaskStatus).toBe("completed");
    expect(result[0].pmReport).toBe(report);
  });

  it("still replaces a live partial draft with the terminal answer", () => {
    const result = reconcilePmHistoryTerminalAssistant(
      [assistant("流式草稿")],
      "最终完整答案",
      { taskId: "task-1", taskStatus: "completed" },
    );

    expect(result[0].content).toBe("最终完整答案");
  });

  it("keeps the durable final-delivery projection on the historical reply", () => {
    const artifact = {
      schemaVersion: "pm-final-delivery-v1",
      taskId: "task-1",
      taskStatus: "completed",
      qualityStatus: "passed",
      deliveryStatus: "persisted",
      response: { text: "最终完整答案" },
      stages: [],
      contentHash: "hash",
    };
    const result = reconcilePmHistoryTerminalAssistant(
      [assistant("最终完整答案")],
      "最终完整答案",
      {
        taskId: "task-1",
        taskStatus: "completed",
        pmFinalDelivery: artifact,
      },
    );
    expect(result[0].pmFinalDelivery).toBe(artifact);
  });
});
