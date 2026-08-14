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
  diagnosticOnly: boolean;
  recoveryNote?: string;
}

interface Nl2sqlExecutionAttempt {
  attempt: number;
  status: string;
  sql?: string;
  executionMs?: number;
  error?: string;
  retryReason?: string;
  repairStrategy?: string;
  scopeChanged: boolean;
  diagnosticOnly: boolean;
  repairRationale?: string;
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

export interface Nl2sqlProgressEvent {
  stage?: string;
  status?: string;
  message?: string;
  executionDetail?: Record<string, unknown>;
  progressNarrative?: string;
  waitElapsedMs?: number;
}

export function nl2sqlProgressEventsFromStageEvents(
  events: Array<{
    stage: string;
    status: string;
    detail?: Record<string, unknown>;
  }>,
): Nl2sqlProgressEvent[] {
  return events
    .filter((event) => event.stage.startsWith("nl2sql_"))
    .map((event) => ({
      stage: event.stage,
      status: event.status,
      message: typeof event.detail?.message === "string" ? event.detail.message : undefined,
      executionDetail:
        event.detail?.executionDetail &&
        typeof event.detail.executionDetail === "object" &&
        !Array.isArray(event.detail.executionDetail)
          ? event.detail.executionDetail as Record<string, unknown>
          : undefined,
      progressNarrative:
        typeof event.detail?.progressNarrative === "string"
          ? event.detail.progressNarrative
          : undefined,
      waitElapsedMs:
        typeof event.detail?.waitElapsedMs === "number"
          ? event.detail.waitElapsedMs
          : undefined,
    }));
}

function progressEventsFromArgs(args: string): Nl2sqlProgressEvent[] {
  try {
    const value = JSON.parse(args) as Record<string, unknown>;
    return Array.isArray(value.__progressEvents)
      ? value.__progressEvents.filter((item): item is Nl2sqlProgressEvent => Boolean(item && typeof item === "object"))
      : [];
  } catch {
    return [];
  }
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
                repairStrategy: optionalString(firstValue(attempt, "repairStrategy", "repair_strategy")),
                scopeChanged: optionalBoolean(firstValue(attempt, "scopeChanged", "scope_changed")) ?? false,
                diagnosticOnly: optionalBoolean(firstValue(attempt, "diagnosticOnly", "diagnostic_only")) ?? false,
                repairRationale: optionalString(firstValue(attempt, "repairRationale", "repair_rationale")),
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
          diagnosticOnly: optionalBoolean(firstValue(step, "diagnosticOnly", "diagnostic_only")) ?? false,
          recoveryNote: optionalString(firstValue(step, "recoveryNote", "recovery_note")),
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
      args: JSON.stringify({
        ...(audit.input && typeof audit.input === "object" && !Array.isArray(audit.input) ? audit.input : {}),
        __progressEvents: audit.progress_events ?? [],
      }),
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

function progressEventKey(event: Nl2sqlProgressEvent): string {
  return JSON.stringify([
    event.stage,
    event.status,
    event.message,
    event.executionDetail,
    event.progressNarrative,
    event.waitElapsedMs,
  ]);
}

function uniqueProgressEvents(events: Nl2sqlProgressEvent[]): Nl2sqlProgressEvent[] {
  const seen = new Set<string>();
  return events.filter((event) => {
    const key = progressEventKey(event);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function stageLabel(stage: string | undefined, t: ReturnType<typeof useTranslation>["t"]): string {
  const normalized = (stage ?? "").replace(/^nl2sql_/, "");
  const labels: Record<string, string> = {
    request_validation: t("chat.nl2sqlAuditStageRequestValidation", "校验请求"),
    sql_knowledge_probe: t("chat.nl2sqlAuditStageKnowledge", "检索 SQL 知识"),
    federated_workspace: t("chat.nl2sqlAuditStageWorkspace", "构建联邦工作区"),
    load_context: t("chat.nl2sqlAuditStageContext", "检索数据上下文"),
    load_schema: t("chat.nl2sqlAuditStageSchema", "加载 Schema"),
    route_selected: t("chat.nl2sqlAuditStageRoute", "选择数据源"),
    query_understanding: t("chat.nl2sqlAuditStageUnderstanding", "理解查询"),
    generate_sql: t("chat.nl2sqlAuditStageGenerating", "生成 SQL"),
    generated_sql: t("chat.nl2sqlAuditStageGenerated", "SQL 已生成"),
    explain_sql: t("chat.nl2sqlAuditStageValidating", "验证 SQL"),
    execute_sql: t("chat.nl2sqlAuditStageExecuting", "执行 SQL"),
    execute_sql_failed: t("chat.nl2sqlAuditStageExecutionFailed", "SQL 执行未通过"),
    retry_sql: t("chat.nl2sqlAuditStageRetrying", "重试 SQL"),
    repair_sql: t("chat.nl2sqlAuditStageRepairing", "修复 SQL"),
    persist_result: t("chat.nl2sqlAuditStageResult", "整理结果"),
    progress_wait: t("chat.nl2sqlAuditStageWaiting", "等待数据源进展"),
  };
  return labels[normalized] ?? (
    normalized.replaceAll("_", " ") ||
    t("chat.nl2sqlAuditRunning", "正在生成并验证 SQL...")
  );
}

function statusLabel(status: string | undefined, t: ReturnType<typeof useTranslation>["t"]): string {
  const normalized = (status ?? "running").toLowerCase();
  const labels: Record<string, string> = {
    pending: t("chat.nl2sqlAuditStatusPending", "等待执行"),
    queued: t("chat.nl2sqlAuditStatusPending", "等待执行"),
    running: t("chat.nl2sqlAuditStatusRunning", "运行中"),
    generated: t("chat.nl2sqlAuditStatusGenerated", "已生成"),
    submitting: t("chat.nl2sqlAuditStatusSubmitting", "提交中"),
    completed: t("chat.nl2sqlAuditStatusCompleted", "已完成"),
    success: t("chat.nl2sqlAuditStatusCompleted", "已完成"),
    failed: t("chat.nl2sqlAuditStatusFailed", "失败"),
    error: t("chat.nl2sqlAuditStatusFailed", "失败"),
  };
  return labels[normalized] ?? status ?? normalized;
}

function Nl2sqlAuditPanelImpl({
  toolCalls,
  progressEvents = [],
}: {
  toolCalls?: ToolCallInfo[];
  progressEvents?: Nl2sqlProgressEvent[];
}) {
  const { t } = useTranslation();
  const calls = useMemo(
    () => (toolCalls ?? []).filter(isNl2sqlAuditTool),
    [toolCalls],
  );
  const visibleCalls = useMemo<ToolCallInfo[]>(
    () => calls.length > 0
      ? calls
      : progressEvents.length > 0
        ? [{
            index: -1,
            name: "nl2sql_analyze",
            source: "builtin",
            args: "{}",
            result: "",
            isError: false,
            status: "running",
          }]
        : [],
    [calls, progressEvents.length],
  );
  if (visibleCalls.length === 0) return null;

  const latest = visibleCalls.at(-1)!;
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
              <Tag color={statusColor(latestStatus, latest.isError)}>{statusLabel(latestStatus, t)}</Tag>
              {latestAudit?.rowCount != null ? <Tag>{t("chat.nl2sqlAuditRows", "{{count}} 行", { count: latestAudit.rowCount })}</Tag> : null}
            </Space>
          ),
          children: (
            <div style={{ display: "grid", gap: 12, minWidth: 0 }}>
              {visibleCalls.map((call, callIndex) => {
                const audit = parseNl2sqlAuditResult(call.result);
                const restoredProgressEvents = progressEventsFromArgs(call.args);
                const visibleProgressEvents = uniqueProgressEvents(callIndex === visibleCalls.length - 1
                  ? [...restoredProgressEvents, ...progressEvents]
                  : restoredProgressEvents);
                const running = call.status === "pending" || call.status === "running";
                return (
                  <div key={`${call.index}-${callIndex}`} style={{ display: "grid", gap: 10, minWidth: 0 }}>
                    {audit?.summary ? <Text>{audit.summary}</Text> : null}
                    {running && visibleProgressEvents.length === 0 ? <Text type="secondary">{t("chat.nl2sqlAuditRunning", "正在生成并验证 SQL...")}</Text> : null}
                    {visibleProgressEvents.map((event, eventIndex) => {
                      const detail = event.executionDetail;
                      const sql = typeof detail?.sql === "string" ? detail.sql : undefined;
                      const queryId = typeof detail?.queryId === "string" ? detail.queryId : undefined;
                      const queryStatus = typeof detail?.status === "string" ? detail.status : undefined;
                      const processedRows = typeof detail?.processedRows === "number" ? detail.processedRows : undefined;
                      const rowCount = typeof detail?.rowCount === "number" ? detail.rowCount : undefined;
                      return (
                        <div key={`${event.stage}-${eventIndex}`} style={{ display: "grid", gap: 6, padding: 10, background: "var(--bg-surface)", border: "1px solid var(--border-subtle)", borderRadius: 6 }}>
                          <Space size={[6, 4]} wrap>
                            <Text strong>{stageLabel(event.stage, t)}</Text>
                            {queryStatus ? <Tag color="processing">{statusLabel(queryStatus, t)}</Tag> : null}
                            {queryId ? <Tag>{t("chat.nl2sqlAuditQueryId", "查询 ID")}: {queryId}</Tag> : null}
                            {processedRows != null ? <Tag>{t("chat.nl2sqlAuditProcessedRows", "已处理 {{count}} 行", { count: processedRows })}</Tag> : null}
                            {rowCount != null ? <Tag color="success">{t("chat.nl2sqlAuditResultRows", "返回 {{count}} 行", { count: rowCount })}</Tag> : null}
                          </Space>
                          {event.progressNarrative ? <Text type="secondary">{event.progressNarrative}</Text> : null}
                          {!event.progressNarrative && event.message ? <Text type="secondary">{event.message}</Text> : null}
                          {sql ? <pre style={{ margin: 0, padding: 10, overflowX: "auto", whiteSpace: "pre-wrap", overflowWrap: "anywhere", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>{sql}</pre> : null}
                          {Array.isArray(detail?.rowsPreview) && detail.rowsPreview.length > 0 ? (
                            <pre style={{ margin: 0, padding: 10, maxHeight: 280, overflow: "auto", whiteSpace: "pre-wrap", overflowWrap: "anywhere", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>
                              {JSON.stringify(detail.rowsPreview, null, 2)}
                            </pre>
                          ) : null}
                          {typeof detail?.error === "string" ? <Text type="danger">{detail.error}</Text> : null}
                        </div>
                      );
                    })}
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
                          {step.rowCount != null ? <Tag>{t("chat.nl2sqlAuditRows", "{{count}} 行", { count: step.rowCount })}</Tag> : null}
                          {step.diagnosticOnly ? (
                            <Tag color="warning">{t("chat.nl2sqlAuditDiagnosticOnly", "仅诊断验证")}</Tag>
                          ) : null}
                        </Space>
                        {step.recoveryNote ? (
                          <Text type="secondary" style={{ display: "block", marginTop: 6 }}>
                            {t("chat.nl2sqlAuditRecoveryNote", "恢复说明")}: {step.recoveryNote}
                          </Text>
                        ) : null}
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
                                    {attempt.repairStrategy ? (
                                      <Tag color="blue">
                                        {t("chat.nl2sqlAuditRecoveryStrategy", "模型恢复策略")}: {attempt.repairStrategy}
                                      </Tag>
                                    ) : null}
                                    {attempt.scopeChanged ? (
                                      <Tag color="warning">{t("chat.nl2sqlAuditScopeChanged", "查询范围已调整")}</Tag>
                                    ) : null}
                                    {attempt.diagnosticOnly ? (
                                      <Tag color="warning">{t("chat.nl2sqlAuditDiagnosticOnly", "仅诊断验证")}</Tag>
                                    ) : null}
                                  </Space>
                                  {attempt.repairRationale ? (
                                    <Text type="secondary" style={{ display: "block", marginTop: 4 }}>
                                      {attempt.repairRationale}
                                    </Text>
                                  ) : null}
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
