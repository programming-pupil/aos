import { describe, expect, it } from "vitest";
import {
  hasNl2sqlAuditToolCalls,
  nl2sqlAuditToolCallsFromHistory,
  parseNl2sqlAuditResult,
} from "./Nl2sqlAuditPanel";
import type { ToolCallInfo } from "./types";

function tool(overrides: Partial<ToolCallInfo> = {}): ToolCallInfo {
  return {
    index: 1,
    name: "nl2sql_analyze",
    source: "builtin",
    args: "{}",
    result: "",
    isError: false,
    status: "success",
    ...overrides,
  };
}

describe("Nl2sqlAuditPanel", () => {
  it("parses the compact parent-agent NL2SQL result", () => {
    const result = parseNl2sqlAuditResult(JSON.stringify({
      status: "completed",
      executionSucceeded: true,
      sqlRecorded: true,
      schemaChecked: true,
      rowCount: 2,
      steps: [{
        stepId: 1,
        description: "查询七日 ROI",
        datasourceId: "ds-1",
        sql: "SELECT dt, roi FROM metrics",
        rowsPreview: [{ dt: "2026-07-23", roi: 1.25 }],
        rowCount: 2,
        executionMs: 83,
        error: null,
        executionAttempts: [{
          attempt: 1,
          status: "failed",
          sql: "SELECT dt, roi FROM metrics",
          executionMs: 1000,
          error: "504 Gateway Timeout",
          retryReason: "transient_retry",
        }, {
          attempt: 2,
          status: "succeeded",
          sql: "SELECT dt, roi FROM metrics",
          executionMs: 83,
          error: null,
          retryReason: "transient_retry",
        }],
      }],
    }));

    expect(result?.executionSucceeded).toBe(true);
    expect(result?.steps[0].sql).toBe("SELECT dt, roi FROM metrics");
    expect(result?.steps[0].executionMs).toBe(83);
    expect(result?.steps[0].rowsPreview).toHaveLength(1);
    expect(result?.steps[0].executionAttempts).toHaveLength(2);
    expect(result?.steps[0].executionAttempts[0].error).toContain("504");
    expect(result?.steps[0].executionAttempts[1].status).toBe("succeeded");
  });

  it("unwraps persisted output envelopes and snake_case fields", () => {
    const result = parseNl2sqlAuditResult(JSON.stringify({
      output: JSON.stringify({
        status: "completed",
        execution_succeeded: true,
        sql_recorded: true,
        schema_checked: true,
        steps: [{
          step_id: "step-2",
          description: "Execute SQL",
          sql: "SELECT 1",
          rows_preview: [{ value: 1 }],
          row_count: 1,
          execution_ms: 12,
        }],
      }),
    }));

    expect(result?.schemaChecked).toBe(true);
    expect(result?.steps[0].stepId).toBe("step-2");
    expect(result?.steps[0].rowCount).toBe(1);
  });

  it("only activates for the managed NL2SQL parent tool", () => {
    expect(hasNl2sqlAuditToolCalls([tool()])).toBe(true);
    expect(hasNl2sqlAuditToolCalls([tool({ name: "workspace_read" })])).toBe(false);
  });

  it("restores failed and successful NL2SQL attempts from durable history", () => {
    const calls = nl2sqlAuditToolCallsFromHistory([
      {
        tool_call_id: "call-1",
        status: "completed",
        result: {
          status: "failed",
          executionSucceeded: false,
          sqlRecorded: true,
          schemaChecked: false,
          steps: [{
            stepId: 1,
            description: "Execute SQL",
            sql: "SELECT 1",
            error: "504 Gateway Timeout",
          }],
        },
      },
      {
        tool_call_id: "call-2",
        status: "completed",
        result: {
          status: "completed",
          executionSucceeded: true,
          sqlRecorded: true,
          schemaChecked: true,
          rowCount: 1,
          steps: [{
            stepId: 1,
            description: "Execute SQL",
            sql: "SELECT 1",
            rowsPreview: [{ value: 1 }],
            rowCount: 1,
          }],
        },
      },
    ]);

    expect(calls).toHaveLength(2);
    expect(calls[0].isError).toBe(true);
    expect(calls[1].isError).toBe(false);
    expect(parseNl2sqlAuditResult(calls[1].result)?.executionSucceeded).toBe(true);
  });
});
