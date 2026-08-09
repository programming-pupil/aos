import { Collapse, Space, Tag, Typography } from "antd";
import { memo, useMemo } from "react";
import { useTranslation } from "react-i18next";
import type { SuperAssistantNl2sqlAudit } from "@/types";
import type { ToolCallInfo } from "./types";

const { Text } = Typography;

interface Nl2sqlAuditStep {
  stepId: string | number;
  description: string;
  datasourceId?: string;
  sql?: string;
  columns: string[];
  rowsPreview: unknown[];
  rowCount?: number;
  executionMs?: number;
  error?: string;
  executionAttempts: Nl2sqlExecutionAttempt[];
}

interface Nl2sqlExecutionAttempt {
  attempt: number;
  status: string;
  sql?: string;
  executionMs?: number;
  error?: string;
  retryReason?: string;
}

export interface Nl2sqlAuditResult {
  status: string;
  summary?: string;
  queryId?: string;
  executionSucceeded?: boolean;
  sqlRecorded?: boolean;
  schemaChecked?: boolean;
  rowCount?: number;
  columns: string[];
  rowsPreview: unknown[];
  usedReferences: unknown[];
  steps: Nl2sqlAuditStep[];
  failedStepCount: number;
  error?: string;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function firstValue(record: Record<string, unknown>, ...keys: string[]): unknown {
  for (const key of keys) {
    if (Object.prototype.hasOwnProperty.call(record, key)) return record[key];
  }
  return undefined;
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function optionalNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

function optionalBoolean(value: unknown): boolean | undefined {
  return typeof value === "boolean" ? value : undefined;
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : [];
}

function unwrapJson(raw: unknown): unknown {
  let value = raw;
  for (let depth = 0; depth < 5; depth += 1) {
    if (typeof value === "string") {
      const text = value.trim().replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/, "");
      if (!text) return null;
      try {
        value = JSON.parse(text);
        continue;
      } catch {
        return value;
      }
    }
    const record = asRecord(value);
    if (!record) return value;
    if (
      Array.isArray(firstValue(record, "steps")) ||
      firstValue(record, "sqlRecorded", "sql_recorded") !== undefined ||
      firstValue(record, "executionSucceeded", "execution_succeeded") !== undefined
    ) {
      return record;
    }
    const nested = firstValue(record, "result", "output", "data", "payload");
    if (nested === undefined || nested === value) return record;
    value = nested;
  }
  return value;
}

export function parseNl2sqlAuditResult(raw: unknown): Nl2sqlAuditResult | null {
  const record = asRecord(unwrapJson(raw));
  if (!record) return null;
  const rawSteps = firstValue(record, "steps");
  const steps = Array.isArray(rawSteps)
    ? rawSteps.flatMap((value, index) => {
        const step = asRecord(value);
        if (!step) return [];
        const rawAttempts = firstValue(step, "executionAttempts", "execution_attempts");
        const executionAttempts = Array.isArray(rawAttempts)
          ? rawAttempts.flatMap((value, attemptIndex) => {
              const attempt = asRecord(value);
              if (!attempt) return [];
              return [{
                attempt: optionalNumber(firstValue(attempt, "attempt")) ?? attemptIndex + 1,
                status: optionalString(firstValue(attempt, "status")) ?? "unknown",
                sql: optionalString(firstValue(attempt, "sql")),
                executionMs: optionalNumber(firstValue(attempt, "executionMs", "execution_ms")),
                error: optionalString(firstValue(attempt, "error")),
                retryReason: optionalString(firstValue(attempt, "retryReason", "retry_reason")),
              }];
            })
          : [];
        return [{
          stepId: (firstValue(step, "stepId", "step_id") as string | number | undefined) ?? index + 1,
          description: optionalString(firstValue(step, "description")) ?? `Step ${index + 1}`,
          datasourceId: optionalString(firstValue(step, "datasourceId", "datasource_id")),
          sql: optionalString(firstValue(step, "sql")),
          columns: stringArray(firstValue(step, "columns")),
          rowsPreview: Array.isArray(firstValue(step, "rowsPreview", "rows_preview"))
            ? (firstValue(step, "rowsPreview", "rows_preview") as unknown[])
            : [],
          rowCount: optionalNumber(firstValue(step, "rowCount", "row_count")),
          executionMs: optionalNumber(firstValue(step, "executionMs", "execution_ms")),
          error: optionalString(firstValue(step, "error")),
          executionAttempts,
        }];
      })
    : [];

  const looksLikeAudit = steps.length > 0 || [
    "sqlRecorded",
    "sql_recorded",
    "executionSucceeded",
    "execution_succeeded",
    "schemaChecked",
    "schema_checked",
  ].some((key) => Object.prototype.hasOwnProperty.call(record, key));
  if (!looksLikeAudit) return null;

  return {
    status: optionalString(firstValue(record, "status")) ?? "completed",
    summary: optionalString(firstValue(record, "summary")),
    queryId: optionalString(firstValue(record, "queryId", "query_id")),
    executionSucceeded: optionalBoolean(firstValue(record, "executionSucceeded", "execution_succeeded")),
    sqlRecorded: optionalBoolean(firstValue(record, "sqlRecorded", "sql_recorded")),
    schemaChecked: optionalBoolean(firstValue(record, "schemaChecked", "schema_checked")),
    rowCount: optionalNumber(firstValue(record, "rowCount", "row_count")),
    columns: stringArray(firstValue(record, "columns")),
    rowsPreview: Array.isArray(firstValue(record, "rowsPreview", "rows_preview"))
      ? (firstValue(record, "rowsPreview", "rows_preview") as unknown[])
      : [],
    usedReferences: Array.isArray(firstValue(record, "usedReferences", "used_references"))
      ? (firstValue(record, "usedReferences", "used_references") as unknown[])
      : [],
    steps,
    failedStepCount: optionalNumber(firstValue(record, "failedStepCount", "failed_step_count")) ??
      steps.filter((step) => Boolean(step.error)).length,
    error: optionalString(firstValue(record, "error")),
  };
}

export function isNl2sqlAuditTool(tool: Pick<ToolCallInfo, "name">): boolean {
  const name = tool.name.toLowerCase().replace(/[\s-]+/g, "_");
  return name === "nl2sql_analyze" || name.endsWith("__nl2sql_analyze");
}

export function hasNl2sqlAuditToolCalls(toolCalls?: ToolCallInfo[]): boolean {
  return Boolean(toolCalls?.some(isNl2sqlAuditTool));
}

export function nl2sqlAuditToolCallsFromHistory(
  audits?: SuperAssistantNl2sqlAudit[] | null,
): ToolCallInfo[] {
  return (audits ?? []).map((audit, index) => {
    const fallbackResult = {
      status: audit.status || "failed",
      executionSucceeded: false,
      sqlRecorded: false,
      schemaChecked: false,
      steps: [],
      error: audit.error_message || undefined,
    };
    const resultPayload = audit.result ?? fallbackResult;
    const parsed = parseNl2sqlAuditResult(resultPayload);
    const normalizedStatus = (audit.status || "failed").toLowerCase();
    const isError =
      normalizedStatus !== "completed" ||
      parsed?.executionSucceeded === false ||
      parsed?.status === "failed" ||
      parsed?.status === "error";
    return {
      index: 100_000 + index,
      name: "nl2sql_analyze",
      source: "builtin",
      args: audit.input == null ? "{}" : JSON.stringify(audit.input),
      result:
        typeof resultPayload === "string"
          ? resultPayload
          : JSON.stringify(resultPayload),
      isError,
      status: isError ? "error" : "success",
    };
  });
}

function statusColor(status: string, isError: boolean): string {
  if (isError || status === "failed" || status === "error") return "error";
  if (status === "pending" || status === "running" || status === "queued") return "processing";
  return "success";
}

function referenceLabel(value: unknown): string {
  if (typeof value === "string") return value;
  const record = asRecord(value);
  if (!record) return String(value ?? "");
  return optionalString(firstValue(record, "filename", "name", "path", "title", "id")) ??
    JSON.stringify(record);
}

function Nl2sqlAuditPanelImpl({ toolCalls }: { toolCalls?: ToolCallInfo[] }) {
  const { t } = useTranslation();
  const calls = useMemo(
    () => (toolCalls ?? []).filter(isNl2sqlAuditTool),
    [toolCalls],
  );
  if (calls.length === 0) return null;

  const latest = calls.at(-1)!;
  const latestAudit = parseNl2sqlAuditResult(latest.result);
  const latestStatus = latest.isError
    ? "failed"
    : latestAudit?.status ?? latest.status;

  return (
    <div style={{ marginTop: 8, border: "1px solid var(--border-subtle)", borderRadius: 8, overflow: "hidden", background: "var(--bg-elevated)" }}>
      <Collapse
        ghost
        size="small"
        items={[{
          key: "nl2sql-audit",
          label: (
            <Space size={[6, 4]} wrap>
              <Text strong>{t("chat.nl2sqlAuditTitle", "NL2SQL 执行记录")}</Text>
              <Tag color={statusColor(latestStatus, latest.isError)}>{latestStatus}</Tag>
              {latestAudit?.rowCount != null ? <Tag>{`${latestAudit.rowCount} rows`}</Tag> : null}
            </Space>
          ),
          children: (
            <div style={{ display: "grid", gap: 12, minWidth: 0 }}>
              {calls.map((call, callIndex) => {
                const audit = parseNl2sqlAuditResult(call.result);
                const running = call.status === "pending" || call.status === "running";
                return (
                  <div key={`${call.index}-${callIndex}`} style={{ display: "grid", gap: 10, minWidth: 0 }}>
                    {audit?.summary ? <Text>{audit.summary}</Text> : null}
                    {running ? <Text type="secondary">{t("chat.nl2sqlAuditRunning", "正在生成并验证 SQL...")}</Text> : null}
                    <Space size={[6, 4]} wrap>
                      {audit?.schemaChecked != null ? (
                        <Tag color={audit.schemaChecked ? "success" : "warning"}>
                          {audit.schemaChecked
                            ? t("chat.nl2sqlAuditSchemaChecked", "Schema 已校验")
                            : t("chat.nl2sqlAuditSchemaUnchecked", "Schema 未校验")}
                        </Tag>
                      ) : null}
                      {audit?.executionSucceeded != null ? (
                        <Tag color={audit.executionSucceeded ? "success" : "error"}>
                          {audit.executionSucceeded
                            ? t("chat.nl2sqlAuditExecuted", "执行成功")
                            : t("chat.nl2sqlAuditExecutionFailed", "执行失败")}
                        </Tag>
                      ) : null}
                      {audit?.queryId ? <Tag>{`Query ${audit.queryId}`}</Tag> : null}
                    </Space>
                    {audit?.steps.map((step, stepIndex) => (
                      <div key={`${step.stepId}-${stepIndex}`} style={{ borderTop: stepIndex > 0 ? "1px solid var(--border-subtle)" : undefined, paddingTop: stepIndex > 0 ? 10 : 0, minWidth: 0 }}>
                        <Space size={[6, 4]} wrap>
                          <Text strong>{step.description}</Text>
                          <Tag color={step.error ? "error" : "success"}>
                            {step.error ? t("chat.nl2sqlAuditFailed", "失败") : t("chat.nl2sqlAuditCompleted", "已执行")}
                          </Tag>
                          {step.datasourceId ? <Tag>{step.datasourceId}</Tag> : null}
                          {step.executionMs != null ? <Tag>{`${step.executionMs} ms`}</Tag> : null}
                          {step.rowCount != null ? <Tag>{`${step.rowCount} rows`}</Tag> : null}
                        </Space>
                        {step.sql ? (
                          <div style={{ marginTop: 8 }}>
                            <Text strong>{t("chat.nl2sqlAuditSql", "SQL")}</Text>
                            <pre style={{ margin: "4px 0 0", padding: 10, overflowX: "auto", whiteSpace: "pre-wrap", overflowWrap: "anywhere", background: "var(--bg-surface)", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>
                              {step.sql}
                            </pre>
                          </div>
                        ) : null}
                        {step.executionAttempts.length > 0 ? (
                          <div style={{ marginTop: 8, display: "grid", gap: 6 }}>
                            <Text strong>{t("chat.nl2sqlAuditAttempts", "执行尝试")}</Text>
                            {step.executionAttempts.map((attempt, attemptIndex) => {
                              const succeeded = ["succeeded", "success", "completed"].includes(
                                attempt.status.toLowerCase(),
                              ) && !attempt.error;
                              return (
                                <div
                                  key={`${attempt.attempt}-${attemptIndex}`}
                                  style={{
                                    padding: "8px 10px",
                                    borderLeft: `3px solid ${succeeded ? "var(--color-success)" : "var(--color-error)"}`,
                                    background: "var(--bg-surface)",
                                    minWidth: 0,
                                  }}
                                >
                                  <Space size={[6, 4]} wrap>
                                    <Text>{t("chat.nl2sqlAuditAttempt", "第 {{attempt}} 次", { attempt: attempt.attempt })}</Text>
                                    <Tag color={succeeded ? "success" : "error"}>
                                      {succeeded
                                        ? t("chat.nl2sqlAuditCompleted", "已执行")
                                        : t("chat.nl2sqlAuditFailed", "失败")}
                                    </Tag>
                                    {attempt.executionMs != null ? <Tag>{`${attempt.executionMs} ms`}</Tag> : null}
                                    {attempt.retryReason ? (
                                      <Tag color="processing">
                                        {attempt.retryReason.startsWith("sql_repair:")
                                          ? t("chat.nl2sqlAuditSqlRepairRetry", "SQL 修复后重试")
                                          : t("chat.nl2sqlAuditTransientRetry", "瞬时故障重试")}
                                      </Tag>
                                    ) : null}
                                  </Space>
                                  {attempt.sql && attempt.sql !== step.sql ? (
                                    <pre style={{ margin: "6px 0 0", padding: 8, overflowX: "auto", whiteSpace: "pre-wrap", overflowWrap: "anywhere", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>
                                      {attempt.sql}
                                    </pre>
                                  ) : null}
                                  {attempt.error ? <Text type="danger" style={{ display: "block", marginTop: 4 }}>{attempt.error}</Text> : null}
                                </div>
                              );
                            })}
                          </div>
                        ) : null}
                        {step.rowsPreview.length > 0 ? (
                          <div style={{ marginTop: 8 }}>
                            <Text strong>{t("chat.nl2sqlAuditResult", "执行结果预览")}</Text>
                            <pre style={{ margin: "4px 0 0", padding: 10, maxHeight: 280, overflow: "auto", whiteSpace: "pre-wrap", overflowWrap: "anywhere", background: "var(--bg-surface)", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>
                              {JSON.stringify(step.rowsPreview, null, 2)}
                            </pre>
                          </div>
                        ) : null}
                        {step.error ? <Text type="danger" style={{ display: "block", marginTop: 6 }}>{step.error}</Text> : null}
                      </div>
                    ))}
                    {audit && audit.steps.length === 0 && audit.rowsPreview.length > 0 ? (
                      <pre style={{ margin: 0, padding: 10, maxHeight: 280, overflow: "auto", whiteSpace: "pre-wrap", overflowWrap: "anywhere", background: "var(--bg-surface)", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>
                        {JSON.stringify(audit.rowsPreview, null, 2)}
                      </pre>
                    ) : null}
                    {audit?.usedReferences.length ? (
                      <Text type="secondary">
                        {t("chat.nl2sqlAuditReferences", "引用")}: {audit.usedReferences.map(referenceLabel).filter(Boolean).join(", ")}
                      </Text>
                    ) : null}
                    {audit?.error || (call.isError && call.result) ? (
                      <Text type="danger">{audit?.error ?? call.result}</Text>
                    ) : null}
                  </div>
                );
              })}
            </div>
          ),
        }]}
      />
    </div>
  );
}

export const Nl2sqlAuditPanel = memo(Nl2sqlAuditPanelImpl);
