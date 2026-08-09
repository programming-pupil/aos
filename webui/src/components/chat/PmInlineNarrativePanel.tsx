import { Tag, Typography } from "antd";
import type { TFunction } from "i18next";
import type { PmStageStatus } from "./chatCore.pmTypes";

const { Text } = Typography;

export interface PmInlineActionTrailItem {
  id: string;
  status: "pending" | "running" | "success" | "error";
  summary: string;
  meta: string;
}

export interface PmInlineStageTrailItem {
  id: string;
  label: string;
  status: PmStageStatus;
  text: string;
  detail: string;
  isTail: boolean;
}

export function PmInlineNarrativePanel({
  t,
  leadText,
  actionTrail,
  stageTrail,
}: {
  t: TFunction;
  leadText: string;
  actionTrail: PmInlineActionTrailItem[];
  stageTrail: PmInlineStageTrailItem[];
}) {
  if (
    leadText.trim().length === 0 &&
    actionTrail.length === 0 &&
    stageTrail.length === 0
  ) {
    return null;
  }

  return (
    <div
      style={{
        border: "1px solid var(--border-default)",
        borderRadius: 12,
        background: "var(--bg-elevated)",
        padding: "12px 14px",
        marginTop: 2,
        display: "grid",
        gap: 10,
      }}
    >
      {leadText && (
        <div
          style={{
            border: "1px solid var(--border-subtle)",
            borderRadius: 10,
            background: "var(--bg-surface)",
            padding: "10px 12px",
          }}
        >
          <Text
            style={{
              display: "block",
              fontSize: 17,
              lineHeight: 1.7,
              color: "var(--text-secondary)",
              whiteSpace: "pre-wrap",
            }}
          >
            {leadText}
          </Text>
        </div>
      )}

      {actionTrail.length > 0 && (
        <div
          style={{
            borderTop: "1px solid var(--border-subtle)",
            paddingTop: 10,
            display: "grid",
            gap: 6,
          }}
        >
          {actionTrail.map((action) => (
            <div
              key={action.id}
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: 8,
              }}
            >
              <span
                style={{
                  fontSize: 12,
                  marginTop: 2,
                  color:
                    action.status === "success"
                      ? "var(--color-success)"
                      : action.status === "error"
                        ? "var(--color-error)"
                        : "var(--accent-ai)",
                }}
              >
                {action.status === "running" ? "◌" : "●"}
              </span>
              <div style={{ minWidth: 0, flex: 1 }}>
                <Text
                  style={{
                    display: "block",
                    fontSize: 13,
                    lineHeight: 1.6,
                    color:
                      action.status === "error"
                        ? "var(--color-error)"
                        : "var(--text-secondary)",
                    whiteSpace: "pre-wrap",
                  }}
                >
                  {action.summary}
                </Text>
                {action.meta && (
                  <Text
                    style={{
                      display: "block",
                      marginTop: 2,
                      fontSize: 11,
                      color: "var(--text-muted)",
                    }}
                  >
                    {action.meta}
                  </Text>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {stageTrail.length > 0 && (
        <div>
          <Text
            style={{
              display: "block",
              fontSize: 12,
              color: "var(--text-muted)",
              marginBottom: 6,
            }}
          >
            {t("operations.pmInlinePhaseTrail", "阶段进展")}
          </Text>
          <div style={{ display: "grid", gap: 8 }}>
            {stageTrail.map((stage) => {
              const nodeColor =
                stage.status === "completed"
                  ? "var(--color-success)"
                  : stage.status === "failed"
                    ? "var(--color-error)"
                    : stage.status === "running"
                      ? "var(--accent-ai)"
                      : "var(--text-muted)";
              const lineColor =
                stage.status === "completed"
                  ? "rgba(34, 197, 94, 0.35)"
                  : stage.status === "failed"
                    ? "rgba(248, 113, 113, 0.35)"
                    : "var(--border-subtle)";
              return (
                <div
                  key={`inline-stage-${stage.id}`}
                  style={{
                    display: "grid",
                    gridTemplateColumns: "18px minmax(0, 1fr)",
                    columnGap: 10,
                  }}
                >
                  <div style={{ position: "relative" }}>
                    <span
                      style={{
                        position: "absolute",
                        top: 3,
                        left: 4,
                        width: 10,
                        height: 10,
                        borderRadius: "50%",
                        border: `1px solid ${nodeColor}`,
                        background:
                          stage.status === "pending"
                            ? "transparent"
                            : nodeColor,
                      }}
                    />
                    {!stage.isTail && (
                      <span
                        style={{
                          position: "absolute",
                          top: 14,
                          bottom: -8,
                          left: 8,
                          width: 2,
                          borderRadius: 2,
                          background: lineColor,
                        }}
                      />
                    )}
                  </div>
                  <div
                    style={{
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 8,
                      background: "var(--bg-surface)",
                      padding: "6px 8px",
                    }}
                  >
                    <div
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 8,
                        flexWrap: "wrap",
                      }}
                    >
                      <Text style={{ fontSize: 13, color: "var(--text-secondary)" }}>
                        {stage.label}
                      </Text>
                      <Tag
                        color={
                          stage.status === "completed"
                            ? "success"
                            : stage.status === "failed"
                              ? "error"
                              : stage.status === "running"
                                ? "processing"
                                : "default"
                        }
                        style={{ marginRight: 0 }}
                      >
                        {stage.text}
                      </Tag>
                    </div>
                    {stage.detail && (
                      <Text
                        style={{
                          display: "block",
                          marginTop: 3,
                          fontSize: 11,
                          color: "var(--text-muted)",
                          lineHeight: 1.5,
                          whiteSpace: "pre-wrap",
                        }}
                      >
                        {stage.detail}
                      </Text>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
