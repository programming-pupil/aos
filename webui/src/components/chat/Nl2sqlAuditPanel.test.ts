import { describe, expect, it } from "vitest";
import {
  hasNl2sqlAuditToolCalls,
  nl2sqlProgressEventsFromStageEvents,
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

  it("restores model-guided partition recovery and diagnostic scope", () => {
    const result = parseNl2sqlAuditResult({
      status: "completed",
      executionSucceeded: false,
      sqlRecorded: true,
      schemaChecked: false,
      steps: [{
        stepId: 1,
        description: "Validate a recent available partition",
        sql: "SELECT app_id, roi FROM metrics WHERE dt = DATE '2026-08-13'",
        rowCount: 2,
        diagnosticOnly: true,
        recoveryNote: "The requested partition is unavailable; this sample only validates the query path.",
        executionAttempts: [{
          attempt: 2,
          status: "succeeded",
          sql: "SELECT app_id, roi FROM metrics WHERE dt = DATE '2026-08-13'",
          repairStrategy: "recent available partition",
          scopeChanged: true,
          diagnosticOnly: true,
          repairRationale: "Requested partition location does not exist",
        }],
      }],
    });

    expect(result?.executionSucceeded).toBe(false);
    expect(result?.steps[0].diagnosticOnly).toBe(true);
    expect(result?.steps[0].executionAttempts[0].scopeChanged).toBe(true);
    expect(result?.steps[0].executionAttempts[0].repairStrategy).toContain("recent");
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

  it("keeps every ordered NL2SQL stage event for the live execution log", () => {
    const events = nl2sqlProgressEventsFromStageEvents([
      {
        stage: "nl2sql_generated_sql",
        status: "running",
        detail: {
          message: "SQL generated",
          executionDetail: { sql: "SELECT 1", status: "generated" },
        },
      },
      {
        stage: "nl2sql_execute_sql",
        status: "running",
        detail: {
          message: "Query accepted",
          executionDetail: { queryId: "query-1", status: "RUNNING" },
        },
      },
      {
        stage: "retrieve",
        status: "running",
        detail: { message: "unrelated deep-research event" },
      },
      {
        stage: "nl2sql_execute_sql",
        status: "running",
        detail: {
          message: "Query completed",
          executionDetail: {
            queryId: "query-1",
            status: "completed",
            rowCount: 1,
            rowsPreview: [{ value: 1 }],
          },
        },
      },
    ]);

    expect(events).toHaveLength(3);
    expect(events[0].executionDetail?.sql).toBe("SELECT 1");
    expect(events[1].executionDetail?.queryId).toBe("query-1");
    expect(events[2].executionDetail?.rowsPreview).toEqual([{ value: 1 }]);
  });

  it("embeds durable SQL progress in history tool calls", () => {
    const calls = nl2sqlAuditToolCallsFromHistory([{
      tool_call_id: "call-history",
      status: "completed",
      result: {
        status: "completed",
        executionSucceeded: true,
        sqlRecorded: true,
        schemaChecked: true,
        steps: [],
      },
      progress_events: [{
        stage: "nl2sql_execute_sql",
        executionDetail: {
          sql: "SELECT app_id, roi FROM metrics",
          queryId: "query-history",
          status: "completed",
          rowCount: 2,
        },
      }],
    }]);

    const args = JSON.parse(calls[0].args);
    expect(args.__progressEvents).toEqual([
      expect.objectContaining({
        executionDetail: expect.objectContaining({
          sql: "SELECT app_id, roi FROM metrics",
          queryId: "query-history",
          rowCount: 2,
        }),
      }),
    ]);
  });
});
