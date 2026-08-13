import { nl2sqlApi, streamNl2sqlAttributionTask } from "@/api";
import type {
  AttributionAnalyzeResponse,
  AttributionObservation,
  AttributionReport,
  AttributionTaskEvent,
  AttributionTaskStatusResponse,
} from "@/types";
import { Collapse, Space, Tag, Typography } from "antd";
import { memo, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Markdown } from "./markdownRenderer";

const { Text } = Typography;

type Translate = ReturnType<typeof useTranslation>["t"];

function translatedStatus(status: string, t: Translate): string {
  const key = status.toLowerCase();
  const fallbacks: Record<string, string> = {
    queued: "排队中",
    running: "运行中",
    completed: "已完成",
    partial: "部分完成",
    no_data: "无数据",
    clarification_needed: "需要澄清",
    timed_out: "无进展超时",
    failed: "失败",
    cancelled: "已取消",
    generated: "已生成",
    submitting: "提交中",
    finished: "已完成",
  };
  return t(`chat.attributionAuditStatuses.${key}`, fallbacks[key] ?? status);
}

function translatedStage(stage: string | null | undefined, t: Translate): string {
  const value = (stage ?? "queued").toLowerCase();
  const suffixes: Array<[string, string, string]> = [
    ["generated_sql", "sqlGenerated", "SQL 已生成"],
    ["execute_sql", "sqlExecuting", "正在执行 SQL"],
    ["generate_sql", "sqlGenerating", "正在生成 SQL"],
    ["explain_sql", "sqlValidating", "正在验证 SQL"],
    ["load_schema", "schemaLoading", "正在加载 Schema"],
    ["load_context", "contextLoading", "正在检索数据上下文"],
    ["request_validation", "requestValidating", "正在校验请求"],
    ["synthesize", "synthesizing", "正在汇总结论"],
    ["understand", "understanding", "正在理解归因问题"],
    ["plan", "planning", "正在规划下钻路径"],
    ["diagnose", "diagnosing", "正在继续下钻"],
    ["queued", "queued", "等待执行"],
    ["completed", "completed", "归因完成"],
    ["partial", "partial", "基于现有证据完成"],
    ["no_data", "noData", "未取得可用数据"],
    ["timed_out", "timedOut", "查询长期无响应"],
    ["failed", "failed", "归因失败"],
    ["cancelled", "cancelled", "归因已取消"],
  ];
  const matched = suffixes.find(([suffix]) => value.includes(suffix));
  return matched
    ? t(`chat.attributionAuditStages.${matched[1]}`, matched[2])
    : t("chat.attributionAuditStages.running", "正在执行归因分析");
}

function translatedConfidence(value: string, t: Translate): string {
  const normalized = value.trim().toLowerCase();
  const key = normalized === "高" || normalized === "high"
    ? "high"
    : normalized === "中" || normalized === "medium"
      ? "medium"
      : normalized === "低" || normalized === "low"
        ? "low"
        : "unknown";
  return t(`chat.attributionAuditConfidenceLevels.${key}`, value);
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  if (value < 1024 * 1024 * 1024) return `${(value / 1024 / 1024).toFixed(1)} MB`;
  return `${(value / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

function observationKey(observation: AttributionObservation): string {
  return observation.stepId || `${observation.title}:${observation.question}`;
}

function mergeObservations(
  current: AttributionObservation[],
  incoming: AttributionObservation[],
): AttributionObservation[] {
  const merged = new Map(current.map((item) => [observationKey(item), item]));
  for (const item of incoming) {
    const key = observationKey(item);
    const previous = merged.get(key);
    merged.set(key, previous && previous.sqls.length > item.sqls.length
      ? previous
      : { ...previous, ...item });
  }
  return Array.from(merged.values());
}

function observationFromEvent(
  event: AttributionTaskEvent,
): AttributionObservation | null {
  const item = event.observation;
  if (!item) return null;
  return item;
}

function statusColor(status: string): string {
  if (status === "completed") return "success";
  if (status === "failed" || status === "timed_out" || status === "cancelled") return "error";
  if (
    status === "partial" ||
    status === "no_data" ||
    status === "clarification_needed"
  ) return "warning";
  if (status === "running") return "processing";
  return "default";
}

export function isAttributionTaskTerminalStatus(status: string): boolean {
  return [
    "completed",
    "clarification_needed",
    "no_data",
    "partial",
    "timed_out",
    "failed",
    "cancelled",
  ].includes(status.toLowerCase());
}

export function shouldShowAttributionPreparing(
  running: boolean,
  observationCount: number,
  executionDetailCount: number,
): boolean {
  return running && observationCount === 0 && executionDetailCount === 0;
}

function attributionEventKey(event: AttributionTaskEvent): string {
  return [
    event.task_id,
    event.status,
    event.stage ?? "",
    event.message ?? "",
    event.elapsed_ms,
    event.step_index ?? "",
    event.observation?.stepId ?? "",
  ].join(":");
}

function appendAttributionEvent(
  current: AttributionTaskEvent[],
  event: AttributionTaskEvent,
): AttributionTaskEvent[] {
  const key = attributionEventKey(event);
  if (current.some((item) => attributionEventKey(item) === key)) return current;
  // A complete attribution normally emits fewer than 100 events. The larger
  // cap keeps observations from early steps available even when each NL2SQL
  // branch emits detailed schema/generation/repair progress.
  return [...current, event].slice(-512);
}

function statusFromAttributionEvent(
  taskId: string,
  event: AttributionTaskEvent,
  previous: AttributionTaskStatusResponse | null,
): AttributionTaskStatusResponse {
  return {
    ...(previous ?? { taskId, status: event.status, elapsedMs: event.elapsed_ms }),
    taskId,
    status: event.status,
    stage: event.stage,
    message: event.message,
    elapsedMs: event.elapsed_ms,
    stageElapsedMs: event.stage_elapsed_ms,
    progressPercent: event.progress_percent,
    stepIndex: event.step_index,
    stepTotal: event.step_total,
    observation: event.observation,
    response: event.response ?? previous?.response,
    error: event.error ?? previous?.error,
  };
}

function reportMarkdown(
  report: AttributionReport,
  labels: {
    metricAnswer: string;
    mainCauses: string;
    recommendations: string;
    caveats: string;
    nextQuestions: string;
    coverage: string;
    confidence: string;
    confidenceValue: (value: string) => string;
  },
): string {
  const sections = [
    report.title ? `### ${report.title}` : "",
    report.executiveSummary,
    report.metricAnswer ? `### ${labels.metricAnswer}\n\n${report.metricAnswer}` : "",
    report.mainCauses?.length
      ? `### ${labels.mainCauses}\n\n${report.mainCauses
          .map((item) => {
            const detail = [item.explanation, item.impact, item.confidence]
              .filter(Boolean)
              .join(" · ");
            return `- **${item.title}**${detail ? `: ${detail}` : ""}`;
          })
          .join("\n")}`
      : "",
    report.recommendations?.length
      ? `### ${labels.recommendations}\n\n${report.recommendations.map((item) => `- ${item}`).join("\n")}`
      : "",
    report.caveats?.length
      ? `### ${labels.caveats}\n\n${report.caveats.map((item) => `- ${item}`).join("\n")}`
      : "",
    report.nextQuestions?.length
      ? `### ${labels.nextQuestions}\n\n${report.nextQuestions.map((item) => `- ${item}`).join("\n")}`
      : "",
    report.coverage ? `**${labels.coverage}**: ${report.coverage}` : "",
    report.confidence
      ? `**${labels.confidence}**: ${labels.confidenceValue(report.confidence)}`
      : "",
  ];
  return sections.filter((section) => section.trim()).join("\n\n");
}

export function AttributionMarkdownDetail({ children }: { children: string }) {
  return (
    <div style={{ color: "var(--text-secondary)", minWidth: 0 }}>
      <Markdown relaxed suppressHr>
        {normalizeAttributionMarkdown(children)}
      </Markdown>
    </div>
  );
}

/**
 * Attribution workers occasionally persist a complete Markdown fragment as a
 * single line. Restore only unambiguous block boundaries here so ordinary chat
 * rendering is not affected by specialist payload quirks.
 */
export function normalizeAttributionMarkdown(source: string): string {
  if (!source) return source;

  const normalizeProse = (value: string): string => {
    let normalized = value.replace(/\r\n?/g, "\n");
    if (!normalized.includes("\n") && normalized.includes("\\n")) {
      normalized = normalized.replace(/\\n/g, "\n");
    }
    normalized = normalized
      .replace(
        /(^|[ \t]+)(用户|助手|User|Assistant)\s*[:：][ \t]*/gim,
        (_match, prefix: string, role: string) =>
          `${prefix ? "\n\n" : ""}**${role}:**\n\n`,
      )
      .replace(/([^\n])[ \t]+(#{1,6})[ \t]*(?=[\p{L}\p{N}])/gu, "$1\n\n$2 ")
      .replace(/([。！？!?；;：:])[ \t]+(?=[-*][ \t]+)/g, "$1\n\n")
      .replace(
        /[ \t]*\$\$([\s\S]*?)\$\$[ \t]*/g,
        (_match, expression: string) => `\n\n$$${expression.trim()}$$\n\n`,
      );
    return normalized.replace(/\n{4,}/g, "\n\n\n").trim();
  };

  return source
    .split(/(```[\s\S]*?```)/g)
    .map((segment, index) => (index % 2 === 1 ? segment : normalizeProse(segment)))
    .join("");
}

function AttributionObservationBlock({
  observation,
}: {
  observation: AttributionObservation;
}) {
  const { t } = useTranslation();
  const rows = observation.rows ?? [];
  const hasRows = (observation.rowCount ?? rows.length) > 0;
  return (
    <Collapse
      ghost
      size="small"
      items={[
        {
          key: "detail",
          label: (
            <Space size={[6, 4]} wrap>
              <Text strong>{observation.title || observation.stepId}</Text>
              <Tag color={observation.error ? "error" : hasRows ? "success" : "warning"}>
                {observation.error
                  ? t("chat.attributionAuditFailed", "失败")
                  : hasRows
                    ? t("chat.attributionAuditCompleted", "已执行")
                    : t("chat.attributionAuditNoData", "无数据")}
              </Tag>
              <Tag>{t("chat.attributionAuditRows", "{{count}} 行", { count: observation.rowCount ?? rows.length })}</Tag>
              {observation.sampled ? <Tag>{t("chat.attributionAuditSampled", "预览")}</Tag> : null}
            </Space>
          ),
          children: (
            <div style={{ display: "grid", gap: 10 }}>
              {observation.purpose ? <AttributionMarkdownDetail>{observation.purpose}</AttributionMarkdownDetail> : null}
              {observation.question ? (
                <AttributionMarkdownDetail>{observation.question}</AttributionMarkdownDetail>
              ) : null}
              {observation.datasourceIds?.length ? (
                <Text type="secondary">
                  {t("chat.attributionAuditDatasources", "数据源")}: {observation.datasourceIds.join(", ")}
                </Text>
              ) : null}
              {observation.timeContext ? (
                <Text type="secondary">
                  {t("chat.attributionAuditTimeContext", "时间口径")}: {observation.timeContext}
                </Text>
              ) : null}
              {observation.error ? <Text type="danger">{observation.error}</Text> : null}
              {observation.sqls?.map((sql, index) => (
                <div key={`${observation.stepId}-sql-${index}`}>
                  <Text strong>{t("chat.attributionAuditSql", "SQL")}{observation.sqls.length > 1 ? ` ${index + 1}` : ""}</Text>
                  <pre style={{ margin: "4px 0 0", padding: 10, overflowX: "auto", whiteSpace: "pre-wrap", background: "var(--bg-surface)", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>
                    {sql}
                  </pre>
                </div>
              ))}
              {rows.length > 0 ? (
                <div>
                  <Text strong>{t("chat.attributionAuditResult", "执行结果预览")}</Text>
                  <pre style={{ margin: "4px 0 0", padding: 10, maxHeight: 280, overflow: "auto", whiteSpace: "pre-wrap", background: "var(--bg-surface)", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>
                    {JSON.stringify(rows.slice(0, 12), null, 2)}
                  </pre>
                </div>
              ) : null}
              {observation.usedReferences?.length ? (
                <Text type="secondary">
                  {t("chat.attributionAuditReferences", "引用")}: {observation.usedReferences.map((item) => item.filename).filter(Boolean).join(", ")}
                </Text>
              ) : null}
            </div>
          ),
        },
      ]}
    />
  );
}

function AttributionAuditPanelImpl({
  taskId,
  live = false,
}: {
  taskId: string;
  live?: boolean;
}) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<AttributionTaskStatusResponse | null>(null);
  const [events, setEvents] = useState<AttributionTaskEvent[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let abort: (() => void) | undefined;
    setStatus(null);
    setEvents([]);
    setError(null);
    const attachStream = () => {
      abort = streamNl2sqlAttributionTask(taskId, {
        onEvent: (event) => {
          if (disposed) return;
          setEvents((previous) => appendAttributionEvent(previous, event));
          setStatus((previous) => statusFromAttributionEvent(taskId, event, previous));
        },
        onDone: (event) => {
          if (disposed) return;
          setEvents((previous) => appendAttributionEvent(previous, event));
          setStatus((previous) => statusFromAttributionEvent(taskId, event, previous));
        },
        onError: (reason) => {
          if (!disposed) setError(reason);
        },
      });
    };
    void nl2sqlApi.getAttributionTaskStatus(taskId).then((next) => {
      if (disposed) return;
      setStatus(next);
      // The server durably replays attribution progress, including completed
      // and failed tasks. Always attach once so a refreshed historical session
      // restores every executed observation instead of showing only the final
      // snapshot (or the last failed step).
      attachStream();
    }).catch((reason) => {
      if (disposed) return;
      setError(reason instanceof Error ? reason.message : String(reason));
      if (live) attachStream();
    });
    return () => {
      disposed = true;
      abort?.();
    };
  }, [live, taskId]);

  const response = status?.response as AttributionAnalyzeResponse | null | undefined;
  const observations = useMemo(() => {
    const fromEvents = events.flatMap((event) => {
      const observation = observationFromEvent(event);
      return observation ? [observation] : [];
    });
    return mergeObservations(fromEvents, response?.observations ?? []);
  }, [events, response?.observations]);
  const timeline = useMemo(() => {
    const seen = new Set<string>();
    return events.filter((event) => {
      const key = `${event.stage}:${event.status}:${event.message}:${event.step_index ?? ""}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return Boolean(event.message || event.stage);
    }).slice(-40);
  }, [events]);
  const executionDetails = useMemo(() => events.flatMap((event, index) => {
    const detail = event.detail;
    if (!detail || typeof detail !== "object") return [];
    const kind = typeof detail.kind === "string" ? detail.kind : "progress";
    return [{ event, detail, kind, index }];
  }), [events]);
  const currentStatus = status?.status ?? events.at(-1)?.status ?? "queued";
  const running = ["queued", "running"].includes(currentStatus);
  const taskError = status?.error ?? error;

  return (
    <div style={{ marginTop: 8, border: "1px solid var(--border-subtle)", borderRadius: 8, overflow: "hidden", background: "var(--bg-elevated)" }}>
      <Collapse
        key={taskId}
        ghost
        size="small"
        defaultActiveKey={live ? ["audit"] : []}
        items={[{
          key: "audit",
          label: (
            <Space size={[6, 4]} wrap>
              <Text strong>{t("chat.attributionAuditTitle", "数据归因执行记录")}</Text>
              <Tag color={statusColor(currentStatus)}>{translatedStatus(currentStatus, t)}</Tag>
              {observations.length ? <Tag>{t("chat.attributionAuditSteps", "{{count}} 个步骤", { count: observations.length })}</Tag> : null}
            </Space>
          ),
          children: taskError && observations.length === 0 ? (
            <Text type="danger">{taskError}</Text>
          ) : (
            <div style={{ display: "grid", gap: 10 }}>
              {timeline.length > 0 ? (
                <div style={{ display: "grid", gap: 4 }}>
                  {timeline.map((event, index) => (
                    <Space key={`${event.stage}-${index}`} size={8} wrap>
                      <Tag color={statusColor(event.status)}>{translatedStage(event.stage, t)}</Tag>
                      {event.elapsed_ms > 0 ? <Text type="secondary">{t("chat.attributionAuditElapsed", "已用时 {{seconds}} 秒", { seconds: Math.round(event.elapsed_ms / 1000) })}</Text> : null}
                    </Space>
                  ))}
                </div>
              ) : null}
              {executionDetails.map(({ event, detail, kind, index }) => {
                const sql = typeof detail.sql === "string" ? detail.sql : undefined;
                const queryId = typeof detail.queryId === "string" ? detail.queryId : undefined;
                const queryStatus = typeof detail.status === "string" ? detail.status : undefined;
                const processedRows = typeof detail.processedRows === "number" ? detail.processedRows : undefined;
                const processedBytes = typeof detail.processedBytes === "number" ? detail.processedBytes : undefined;
                const completedSplits = typeof detail.completedSplits === "number" ? detail.completedSplits : undefined;
                const totalSplits = typeof detail.totalSplits === "number" ? detail.totalSplits : undefined;
                const rowCount = typeof detail.rowCount === "number" ? detail.rowCount : undefined;
                return (
                  <div key={`${event.stage}-${kind}-${index}`} style={{ display: "grid", gap: 6, padding: 10, background: "var(--bg-surface)", border: "1px solid var(--border-subtle)", borderRadius: 6 }}>
                    <Space size={[6, 4]} wrap>
                      <Text strong>{translatedStage(event.stage, t)}</Text>
                      {queryStatus ? <Tag color={statusColor(queryStatus.toLowerCase())}>{translatedStatus(queryStatus, t)}</Tag> : null}
                      {queryId ? <Tag>{t("chat.attributionAuditQueryId", "查询 ID")}: {queryId}</Tag> : null}
                      {completedSplits != null && totalSplits != null ? <Tag>{t("chat.attributionAuditSplits", "分片 {{completed}} / {{total}}", { completed: completedSplits, total: totalSplits })}</Tag> : null}
                      {processedRows != null ? <Tag>{t("chat.attributionAuditProcessedRows", "已处理 {{count}} 行", { count: processedRows })}</Tag> : null}
                      {processedBytes != null ? <Tag>{t("chat.attributionAuditProcessedBytes", "已处理 {{size}}", { size: formatBytes(processedBytes) })}</Tag> : null}
                      {rowCount != null ? <Tag color="success">{t("chat.attributionAuditResultRows", "返回 {{count}} 行", { count: rowCount })}</Tag> : null}
                    </Space>
                    {sql ? <pre style={{ margin: 0, padding: 10, overflowX: "auto", whiteSpace: "pre-wrap", overflowWrap: "anywhere", border: "1px solid var(--border-subtle)", borderRadius: 6, fontSize: 12 }}>{sql}</pre> : null}
                  </div>
                );
              })}
              {shouldShowAttributionPreparing(running, observations.length, executionDetails.length) ? <Text type="secondary">{t("chat.attributionAuditRunning", "正在准备 Schema、SQL 和执行结果...")}</Text> : null}
              {observations.map((observation) => <AttributionObservationBlock key={observationKey(observation)} observation={observation} />)}
              {response?.report ? (
                <AttributionMarkdownDetail>
                  {reportMarkdown(response.report, {
                    metricAnswer: t("dataAttribution.metricAnswer", "核心指标"),
                    mainCauses: t("dataAttribution.mainCauses", "主要原因"),
                    recommendations: t("dataAttribution.recommendations", "建议动作"),
                    caveats: t("dataAttribution.caveats", "注意事项"),
                    nextQuestions: t("dataAttribution.nextQuestions", "建议继续追问"),
                    coverage: t("chat.attributionAuditCoverage", "证据覆盖"),
                    confidence: t("chat.attributionAuditConfidence", "结论置信度"),
                    confidenceValue: (value) => translatedConfidence(value, t),
                  })}
                </AttributionMarkdownDetail>
              ) : null}
              {taskError ? <Text type="danger">{taskError}</Text> : null}
            </div>
          ),
        }]}
      />
    </div>
  );
}

export const AttributionAuditPanel = memo(AttributionAuditPanelImpl);
