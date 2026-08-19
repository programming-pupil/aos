import { describe, expect, it, vi } from "vitest";
import type { RuntimeApprovalPaused } from "@/api/agent";
import { approvalResumeHandlers } from "../approvalResume";

const paused: RuntimeApprovalPaused = {
  sessionId: "session-1",
  runtimeTurnId: "turn-1",
  approvals: [],
};

describe("approval resume handlers", () => {
  it("reuses live handlers while a stream is active", () => {
    const existing = { onStreamEnd: vi.fn() };
    const result = approvalResumeHandlers("session-1", {
      sessionId: "session-1",
      handlers: existing,
    }, {
      onApprovalRequired: vi.fn(),
      onStreamEnd: vi.fn(),
      onError: vi.fn(),
    });

    expect(result).toBe(existing);
  });

  it("does not reuse handlers from another session", () => {
    const stale = { onStreamEnd: vi.fn() };
    const onStreamEnd = vi.fn();
    const result = approvalResumeHandlers("session-2", {
      sessionId: "session-1",
      handlers: stale,
    }, {
      onApprovalRequired: vi.fn(),
      onStreamEnd,
      onError: vi.fn(),
    });

    result.onStreamEnd?.(0);

    expect(result).not.toBe(stale);
    expect(stale.onStreamEnd).not.toHaveBeenCalled();
    expect(onStreamEnd).toHaveBeenCalledOnce();
  });

  it("provides callbacks after a page reload", () => {
    const onApprovalRequired = vi.fn();
    const onQuestionRequired = vi.fn();
    const onStreamEnd = vi.fn();
    const onError = vi.fn();
    const result = approvalResumeHandlers("session-1", null, {
      onApprovalRequired,
      onQuestionRequired,
      onStreamEnd,
      onError,
    });

    result.onApprovalRequired?.(paused);
    result.onQuestionRequired?.({
      sessionId: "session-1",
      runtimeTurnId: "turn-1",
      questions: [],
    });
    result.onStreamEnd?.(0);
    result.onError?.("resume failed");

    expect(onApprovalRequired).toHaveBeenCalledWith(paused);
    expect(onQuestionRequired).toHaveBeenCalledOnce();
    expect(onStreamEnd).toHaveBeenCalledOnce();
    expect(onError).toHaveBeenCalledWith("resume failed");
  });
});
