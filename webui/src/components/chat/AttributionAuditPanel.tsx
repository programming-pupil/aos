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
  if (status === "failed" || status === "cancelled") return "error";
  if (status === "running") return "processing";
  return "default";
}

function reportMarkdown(
  report: AttributionReport,
  labels: {
    metricAnswer: string;
    mainCauses: string;
    recommendations: string;
    caveats: string;
    nextQuestions: string;
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
    [report.coverage, report.confidence].filter(Boolean).join(" · "),
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
              <Tag>{`${observation.rowCount ?? rows.length} rows`}</Tag>
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
          setEvents((previous) => [...previous, event].slice(-120));
          if (event.response || event.status === "completed" || event.status === "failed" || event.status === "cancelled") {
            setStatus((previous) => ({
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
              response: event.response,
              error: event.error,
            }));
          }
        },
        onDone: (event) => {
          if (disposed) return;
          setEvents((previous) => [...previous, event].slice(-120));
          setStatus((previous) => ({
            ...(previous ?? { taskId, status: event.status, elapsedMs: event.elapsed_ms }),
            taskId,
            status: event.status,
            stage: event.stage,
            message: event.message,
            elapsedMs: event.elapsed_ms,
            stageElapsedMs: event.stage_elapsed_ms,
            progressPercent: event.progress_percent,
            response: event.response,
            error: event.error,
          }));
        },
        onError: (reason) => {
          if (!disposed) setError(reason);
        },
      });
    };
    void nl2sqlApi.getAttributionTaskStatus(taskId).then((next) => {
      if (disposed) return;
      setStatus(next);
      if (live || ["queued", "running"].includes(next.status)) {
        attachStream();
      }
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
  const currentStatus = status?.status ?? events.at(-1)?.status ?? "queued";
  const running = ["queued", "running"].includes(currentStatus);

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
              <Tag color={statusColor(currentStatus)}>{currentStatus}</Tag>
              {response?.observations?.length ? <Tag>{`${response.observations.length} steps`}</Tag> : null}
            </Space>
          ),
          children: error && observations.length === 0 ? (
            <Text type="danger">{error}</Text>
          ) : (
            <div style={{ display: "grid", gap: 10 }}>
              {timeline.length > 0 ? (
                <div style={{ display: "grid", gap: 4 }}>
                  {timeline.map((event, index) => (
                    <AttributionMarkdownDetail key={`${event.stage}-${index}`}>
                      {event.message || event.stage || ""}
                    </AttributionMarkdownDetail>
                  ))}
                </div>
              ) : null}
              {running && observations.length === 0 ? <Text type="secondary">{t("chat.attributionAuditRunning", "正在准备 Schema、SQL 和执行结果...")}</Text> : null}
              {observations.map((observation) => <AttributionObservationBlock key={observationKey(observation)} observation={observation} />)}
              {response?.report ? (
                <AttributionMarkdownDetail>
                  {reportMarkdown(response.report, {
                    metricAnswer: t("dataAttribution.metricAnswer", "核心指标"),
                    mainCauses: t("dataAttribution.mainCauses", "主要原因"),
                    recommendations: t("dataAttribution.recommendations", "建议动作"),
                    caveats: t("dataAttribution.caveats", "注意事项"),
                    nextQuestions: t("dataAttribution.nextQuestions", "建议继续追问"),
                  })}
                </AttributionMarkdownDetail>
              ) : null}
              {error ? <Text type="danger">{error}</Text> : null}
            </div>
          ),
        }]}
      />
    </div>
  );
}

export const AttributionAuditPanel = memo(AttributionAuditPanelImpl);
