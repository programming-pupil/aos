import { Space, Typography } from "antd";
import type { TFunction } from "i18next";
import { Markdown } from "./markdownRenderer";
import type { PmFinalDeliveryArtifact } from "./chatCore.pmTypes";

const { Text } = Typography;

function looksLikeBlockMarkdown(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return false;
  if (/^#{1,6}\s+/m.test(trimmed) || /```/.test(trimmed)) return true;
  const pipeCount = (trimmed.match(/\|/g) ?? []).length;
  return pipeCount >= 4 && (/(\|\s*:?-{3,}:?\s*){2,}\|/.test(trimmed) || /\|\s+\|/.test(trimmed));
}

export interface PmFinalDeliverySource {
  url: string;
  label: string;
}

export function shouldShowPmFinalDelivery({
  sessionSource,
  executionUiEnabled,
  suppressExecutionUi,
  isStreaming,
  hasAssistantMessage,
  synthStatus,
  backgroundTaskStatus,
  latestTaskStatus,
  deliveryArtifact,
  body,
}: {
  sessionSource: string;
  executionUiEnabled: boolean;
  suppressExecutionUi: boolean;
  isStreaming: boolean;
  hasAssistantMessage: boolean;
  synthStatus?: string | null;
  backgroundTaskStatus?: string | null;
  latestTaskStatus?: string | null;
  deliveryArtifact?: PmFinalDeliveryArtifact;
  body: string;
}): boolean {
  if (
    sessionSource !== "pm" ||
    !executionUiEnabled ||
    suppressExecutionUi ||
    isStreaming ||
    !hasAssistantMessage
  ) {
    return false;
  }
  const normalizedLatestStatus = latestTaskStatus?.toLowerCase() ?? null;
  const artifactReady =
    deliveryArtifact?.deliveryStatus === "persisted" &&
    ["completed", "degraded"].includes(
      deliveryArtifact.taskStatus.toLowerCase(),
    );
  const completed =
    artifactReady ||
    synthStatus === "completed" ||
    backgroundTaskStatus === "completed" ||
    normalizedLatestStatus === "completed";
  if (!completed) return false;
  const text = body.trim();
  if (!text || text.startsWith("研究任务失败：")) return false;
  if (
    text.startsWith("深度分析已启动") ||
    text.toLowerCase().startsWith("deep analysis started")
  ) {
    return false;
  }
  return (
    !normalizedLatestStatus ||
    !["queued", "running", "cancelling", "interrupted"].includes(
      normalizedLatestStatus,
    )
  );
}

export function PmFinalDeliveryPanel({
  t,
  title,
  highlights,
  sources,
  body,
}: {
  t: TFunction;
  title: string;
  highlights: string[];
  sources: PmFinalDeliverySource[];
  body?: string;
}) {
  if (highlights.length === 0 && sources.length === 0 && !body?.trim()) return null;

  return (
    <div
      style={{
        border: "1px solid var(--border-default)",
        borderRadius: 12,
        background: "var(--bg-elevated)",
        padding: "12px 14px",
        display: "grid",
        gap: 10,
      }}
    >
      <div>
        <Text
          style={{
            display: "block",
            fontSize: 12,
            color: "var(--text-muted)",
          }}
        >
          {t("operations.pmFinalDeliveryBadge", "总结交付")}
        </Text>
        <Text
          style={{
            display: "block",
            marginTop: 2,
            fontSize: 20,
            lineHeight: 1.5,
            color: "var(--text-secondary)",
            fontWeight: 600,
            whiteSpace: "pre-wrap",
          }}
        >
          {title}
        </Text>
      </div>

      {body?.trim() && (
        <div
          style={{
            borderTop: "1px solid var(--border-subtle)",
            paddingTop: 10,
            minWidth: 0,
          }}
        >
          <Text style={{ display: "block", fontSize: 12, color: "var(--text-muted)", marginBottom: 6 }}>
            {t("operations.pmFinalDeliveryBody", "完整交付结论")}
          </Text>
          <Markdown relaxed>{body}</Markdown>
        </div>
      )}

      {highlights.length > 0 && (
        <div style={{ display: "grid", gap: 6 }}>
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmFinalDeliveryHighlights", "核心结论摘要")}
          </Text>
          {highlights.map((line, idx) => {
            const blockMarkdown = looksLikeBlockMarkdown(line);
            return (
              <div
                key={`pm-final-highlight-${idx}`}
                style={{
                  display: blockMarkdown ? "block" : "flex",
                  alignItems: "flex-start",
                  gap: 8,
                }}
              >
                {!blockMarkdown && (
                  <span
                    style={{
                      marginTop: 1,
                      color: "var(--accent-ai)",
                      fontSize: 12,
                    }}
                  >
                    ●
                  </span>
                )}
                <div
                  style={{
                    flex: 1,
                    fontSize: 14,
                    color: "var(--text-secondary)",
                    lineHeight: 1.6,
                    minWidth: 0,
                  }}
                >
                  <Markdown inline={!blockMarkdown} relaxed suppressHr>
                    {line}
                  </Markdown>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {sources.length > 0 && (
        <div
          style={{
            borderTop: "1px solid var(--border-subtle)",
            paddingTop: 10,
            display: "grid",
            gap: 6,
          }}
        >
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmFinalDeliverySources", "关键证据来源")}
          </Text>
          <Space size={[6, 6]} wrap>
            {sources.map((source) => (
              <a
                key={`pm-final-src-${source.url}`}
                href={source.url}
                target="_blank"
                rel="noreferrer"
                style={{ fontSize: 12 }}
              >
                {source.label}
              </a>
            ))}
          </Space>
        </div>
      )}
    </div>
  );
}
