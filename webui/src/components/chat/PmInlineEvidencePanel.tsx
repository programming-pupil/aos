import { Space, Tag, Typography } from "antd";
import type { TFunction } from "i18next";
import type { PmClaimEvidence, PmConflictRow } from "./chatCore.pmTypes";

const { Text } = Typography;

export function PmInlineEvidencePanel({
  t,
  claimRows,
  conflictRows,
  selectedClaimIndex,
  selectedClaimEvidence,
  onSelectClaim,
}: {
  t: TFunction;
  claimRows: PmClaimEvidence[];
  conflictRows: PmConflictRow[];
  selectedClaimIndex: number | null;
  selectedClaimEvidence: { row: PmClaimEvidence; excerpt: string } | null;
  onSelectClaim: (index: number) => void;
}) {
  if (claimRows.length === 0 && conflictRows.length === 0) return null;

  return (
    <div
      style={{
        border: "1px solid var(--border-default)",
        borderRadius: 12,
        background: "var(--bg-elevated)",
        padding: "10px 12px",
        display: "grid",
        gap: 10,
      }}
    >
      {claimRows.length > 0 && (
        <div>
          <Text
            style={{
              fontSize: 12,
              color: "var(--text-secondary)",
              display: "block",
              marginBottom: 6,
            }}
          >
            {t("operations.pmClaimEvidenceMap", "结论-证据对齐")}
          </Text>
          <div style={{ display: "grid", gap: 6 }}>
            {claimRows.map((row, idx) => {
              const selected = selectedClaimIndex === idx;
              return (
                <div
                  key={`inline-claim-${idx}-${row.claim}`}
                  role="button"
                  tabIndex={0}
                  onClick={() => onSelectClaim(idx)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      onSelectClaim(idx);
                    }
                  }}
                  style={{
                    border: `1px solid ${selected ? "var(--accent-ai)" : "var(--border-subtle)"}`,
                    borderRadius: 8,
                    padding: "6px 8px",
                    background: selected
                      ? "var(--bg-surface)"
                      : "var(--bg-elevated)",
                    cursor: "pointer",
                  }}
                >
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      flexWrap: "wrap",
                    }}
                  >
                    <Tag
                      color={row.cited ? "success" : "warning"}
                      style={{ marginRight: 0 }}
                    >
                      {row.cited
                        ? t("operations.pmClaimCovered", "已覆盖")
                        : t("operations.pmClaimUncovered", "待补证")}
                    </Tag>
                    <Text
                      style={{ fontSize: 12, color: "var(--text-secondary)" }}
                    >
                      {row.claim}
                    </Text>
                  </div>
                  <Text
                    style={{
                      display: "block",
                      marginTop: 4,
                      fontSize: 11,
                      color: "var(--text-muted)",
                    }}
                  >
                    {t("operations.pmEvidenceLinks", "证据来源")}:{" "}
                    {row.urls.length}
                  </Text>
                </div>
              );
            })}
          </div>
          {selectedClaimEvidence && (
            <div
              style={{
                marginTop: 8,
                border: "1px solid var(--border-subtle)",
                borderRadius: 8,
                background: "var(--bg-surface)",
                padding: "8px 10px",
              }}
            >
              <Text
                style={{
                  display: "block",
                  fontSize: 12,
                  color: "var(--text-secondary)",
                }}
              >
                {t("operations.pmClaimEvidenceExcerpt", "关联原文片段")}
              </Text>
              <Text
                style={{
                  display: "block",
                  marginTop: 4,
                  fontSize: 12,
                  color: "var(--text-muted)",
                  whiteSpace: "pre-wrap",
                  lineHeight: 1.6,
                }}
              >
                {selectedClaimEvidence.excerpt}
              </Text>
              {selectedClaimEvidence.row.urls.length > 0 && (
                <Space size={[6, 6]} wrap style={{ marginTop: 8 }}>
                  {selectedClaimEvidence.row.urls.slice(0, 6).map((url) => (
                    <a
                      key={url}
                      href={url}
                      target="_blank"
                      rel="noreferrer"
                      style={{ fontSize: 11 }}
                    >
                      {url}
                    </a>
                  ))}
                </Space>
              )}
            </div>
          )}
        </div>
      )}

      {conflictRows.length > 0 && (
        <div>
          <Text
            style={{
              fontSize: 12,
              color: "var(--text-secondary)",
              display: "block",
              marginBottom: 6,
            }}
          >
            {t("operations.pmConflictMatrix", "冲突裁决矩阵")}
          </Text>
          <div style={{ display: "grid", gap: 8 }}>
            {conflictRows.map((row, idx) => (
              <div
                key={`inline-conflict-${idx}-${row.topic}-${row.verdict}`}
                style={{
                  border: "1px solid var(--border-subtle)",
                  borderRadius: 8,
                  padding: "8px 10px",
                  background: "var(--bg-surface)",
                }}
              >
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 8,
                    marginBottom: 6,
                    flexWrap: "wrap",
                  }}
                >
                  <Tag color="processing" style={{ marginRight: 0 }}>
                    {row.topic ||
                      t("operations.pmConflictTopicFallback", "主题")}
                  </Tag>
                  <Text
                    style={{ fontSize: 12, color: "var(--text-secondary)" }}
                  >
                    {t("operations.pmConflictVerdict", "裁决")}:{" "}
                    {row.verdict || "-"}
                  </Text>
                </div>
                <div
                  style={{
                    display: "grid",
                    gridTemplateColumns: "1fr 1fr",
                    gap: 8,
                  }}
                >
                  <div
                    style={{
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 6,
                      padding: "6px 8px",
                    }}
                  >
                    <Text
                      style={{
                        fontSize: 11,
                        color: "var(--text-muted)",
                        display: "block",
                      }}
                    >
                      A · {row.source_a || "-"}
                    </Text>
                    <Text
                      style={{
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        whiteSpace: "pre-wrap",
                      }}
                    >
                      {row.claim_a || "-"}
                    </Text>
                  </div>
                  <div
                    style={{
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 6,
                      padding: "6px 8px",
                    }}
                  >
                    <Text
                      style={{
                        fontSize: 11,
                        color: "var(--text-muted)",
                        display: "block",
                      }}
                    >
                      B · {row.source_b || "-"}
                    </Text>
                    <Text
                      style={{
                        fontSize: 12,
                        color: "var(--text-secondary)",
                        whiteSpace: "pre-wrap",
                      }}
                    >
                      {row.claim_b || "-"}
                    </Text>
                  </div>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
