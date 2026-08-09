import { LoadingOutlined } from "@ant-design/icons";
import { Button, Drawer, Space, Tag, Typography } from "antd";
import type { TFunction } from "i18next";
import type {
  PmInlineAction,
  PmSearchUsageSummary,
  PmStageStatus,
  PmToolSummary,
} from "./chatCore.pmTypes";

const { Text } = Typography;

export interface PmExecutionStageView {
  id: string;
  label: string;
  status: PmStageStatus;
  detail: string;
  toolSummary: PmToolSummary | null;
  searchUsage: PmSearchUsageSummary | null;
}

export interface PmExecutionDetailRow {
  id: string;
  label: string;
  displayStatus: PmStageStatus;
  displaySummary: string;
  actions: PmInlineAction[];
  toolSummary: PmToolSummary | null;
  searchUsage: PmSearchUsageSummary | null;
  excerpt: string;
}

export function PmExecutionDrawer({
  t,
  open,
  taskId,
  taskStatus,
  progressPercent,
  currentNarrative,
  recentFindings,
  stages,
  selectedStageId,
  selectedStageLabel,
  showAllDetails,
  detailRows,
  isStreaming,
  onClose,
  onSelectStage,
  onToggleShowAllDetails,
}: {
  t: TFunction;
  open: boolean;
  taskId: string | null;
  taskStatus: string | null;
  progressPercent: number;
  currentNarrative: string;
  recentFindings: string[];
  stages: PmExecutionStageView[];
  selectedStageId: string | null;
  selectedStageLabel: string | null;
  showAllDetails: boolean;
  detailRows: PmExecutionDetailRow[];
  isStreaming: boolean;
  onClose: () => void;
  onSelectStage: (stageId: string) => void;
  onToggleShowAllDetails: () => void;
}) {
  return (
    <Drawer
      title={
        taskId
          ? `${t("operations.pmResearchPanelTitle", "研究执行面板")} · ${taskId.slice(0, 12)}`
          : t("operations.pmResearchPanelTitle", "研究执行面板")
      }
      placement="right"
      width={430}
      open={open}
      onClose={onClose}
      mask={false}
      styles={{
        body: {
          padding: 12,
          background: "var(--bg-surface)",
        },
      }}
    >
      <div
        style={{
          marginBottom: 10,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
          flexWrap: "wrap",
        }}
      >
        <div style={{ minWidth: 200, flex: 1 }}>
          <div
            style={{
              height: 6,
              borderRadius: 99,
              background: "var(--bg-interactive)",
              overflow: "hidden",
            }}
          >
            <div
              style={{
                width: `${progressPercent}%`,
                height: "100%",
                background: "linear-gradient(90deg, #3b82f6, #22c55e)",
                transition: "width 180ms ease",
              }}
            />
          </div>
          <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
            {t("operations.pmProgress", "执行进度")}: {progressPercent}%
            {taskStatus ? ` · ${taskStatus}` : ""}
          </Text>
        </div>
      </div>

      {(currentNarrative || recentFindings.length > 0) && (
        <div
          style={{
            border: "1px solid var(--border-subtle)",
            borderRadius: 10,
            background: "var(--bg-elevated)",
            padding: "10px 12px",
            marginBottom: 10,
          }}
        >
          {currentNarrative && (
            <Text
              style={{
                display: "block",
                fontSize: 12,
                color: "var(--text-secondary)",
                lineHeight: 1.6,
                whiteSpace: "pre-wrap",
              }}
            >
              {currentNarrative}
            </Text>
          )}
          {recentFindings.length > 0 && (
            <div
              style={{
                marginTop: currentNarrative ? 8 : 0,
                display: "grid",
                gap: 6,
              }}
            >
              {recentFindings.map((finding, idx) => (
                <div
                  key={`${idx}-${finding}`}
                  style={{
                    display: "flex",
                    alignItems: "flex-start",
                    gap: 6,
                  }}
                >
                  <span
                    style={{
                      color: "var(--text-muted)",
                      fontSize: 12,
                      marginTop: 1,
                    }}
                  >
                    •
                  </span>
                  <Text
                    style={{
                      fontSize: 12,
                      color: "var(--text-muted)",
                      lineHeight: 1.5,
                      whiteSpace: "pre-wrap",
                    }}
                  >
                    {finding}
                  </Text>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(2, minmax(0, 1fr))",
          gap: 8,
          marginBottom: 10,
        }}
      >
        {stages.map((stage) => {
          const isStageSelected = !showAllDetails && selectedStageId === stage.id;
          return (
            <div
              key={`drawer-${stage.id}`}
              role="button"
              tabIndex={0}
              onClick={() => onSelectStage(stage.id)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  onSelectStage(stage.id);
                }
              }}
              style={{
                border: `1px solid ${isStageSelected ? "var(--accent-ai)" : "var(--border-subtle)"}`,
                borderRadius: 10,
                padding: "8px 10px",
                background: isStageSelected
                  ? "var(--bg-surface)"
                  : "var(--bg-elevated)",
                boxShadow: isStageSelected
                  ? "inset 0 0 0 1px var(--accent-ai)"
                  : "none",
                cursor: "pointer",
                transition: "all 120ms ease",
              }}
            >
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 6,
                }}
              >
                <Text style={{ fontSize: 12, color: "var(--text-secondary)" }}>
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
                  {stage.status === "completed"
                    ? t("operations.statusCompleted", "已完成")
                    : stage.status === "failed"
                      ? t("operations.statusFailed", "失败")
                      : stage.status === "running"
                        ? t("operations.statusRunning", "运行中")
                        : t("common.pending", "待处理")}
                </Tag>
              </div>
              {stage.detail && (
                <Text
                  style={{
                    display: "block",
                    marginTop: 4,
                    fontSize: 11,
                    color: "var(--text-muted)",
                    whiteSpace: "nowrap",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                  }}
                >
                  {stage.detail}
                </Text>
              )}
              {stage.toolSummary && (
                <div style={{ marginTop: 6 }}>
                  <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                    {`${t("operations.pmMetricTools", "工具调用")}: ${stage.toolSummary.count} · ${t("operations.pmMetricErrors", "报错")}: ${stage.toolSummary.errorCount}`}
                  </Text>
                  {stage.toolSummary.byName.length > 0 && (
                    <Space size={[4, 4]} wrap style={{ marginTop: 4 }}>
                      {stage.toolSummary.byName.map((row) => (
                        <Tag
                          key={`drawer-tool-${stage.id}-${row.name}`}
                          style={{ marginRight: 0 }}
                        >
                          {row.name} × {row.count}
                          {row.errorCount > 0
                            ? ` · ${t("operations.pmMetricErrors", "报错")} ${row.errorCount}`
                            : ""}
                        </Tag>
                      ))}
                    </Space>
                  )}
                </div>
              )}
              <PmSearchUsageBadges t={t} usage={stage.searchUsage} />
            </div>
          );
        })}
      </div>

      {(detailRows.length > 0 || !isStreaming) && (
        <div style={{ display: "grid", gap: 8 }}>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 8,
              flexWrap: "wrap",
            }}
          >
            <Space size={[6, 6]} wrap>
              <Text style={{ fontSize: 12, color: "var(--text-secondary)" }}>
                {t("operations.pmStageDetails", "阶段详情")}
              </Text>
              {!showAllDetails && selectedStageLabel && (
                <Tag style={{ marginRight: 0 }}>{selectedStageLabel}</Tag>
              )}
            </Space>
            <Button
              size="small"
              type={showAllDetails ? "primary" : "default"}
              onClick={onToggleShowAllDetails}
            >
              {showAllDetails
                ? t("operations.pmOnlySelectedStage", "仅看所选阶段")
                : t("operations.pmShowAllProcess", "查看全部过程")}
            </Button>
          </div>
          {detailRows.length === 0 && (
            <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
              {t("operations.pmStageDetailsEmpty", "该阶段暂无执行详情")}
            </Text>
          )}
          {detailRows.map((row) => (
            <PmExecutionDetailCard
              key={`drawer-seg-${row.id}`}
              t={t}
              row={row}
            />
          ))}
        </div>
      )}
    </Drawer>
  );
}

function PmExecutionDetailCard({
  t,
  row,
}: {
  t: TFunction;
  row: PmExecutionDetailRow;
}) {
  return (
    <div
      style={{
        border: "1px solid var(--border-subtle)",
        borderRadius: 10,
        background: "var(--bg-elevated)",
        padding: "8px 10px",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        {row.displayStatus === "completed" ? (
          <span style={{ color: "var(--color-success)", fontSize: 12 }}>●</span>
        ) : row.displayStatus === "failed" ? (
          <span style={{ color: "var(--color-error)", fontSize: 12 }}>●</span>
        ) : (
          <LoadingOutlined
            spin
            style={{ color: "var(--accent-ai)", fontSize: 12 }}
          />
        )}
        <Text style={{ fontSize: 13, color: "var(--text-secondary)" }}>
          {row.displaySummary || row.label}
        </Text>
      </div>
      {row.actions.length > 0 && (
        <div style={{ marginTop: 8, display: "grid", gap: 6 }}>
          {row.actions.map((action) => (
            <div
              key={action.id}
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: 8,
                border: "1px solid var(--border-subtle)",
                borderRadius: 8,
                padding: "6px 8px",
                background: "var(--bg-surface)",
              }}
            >
              <span
                style={{
                  fontSize: 11,
                  marginTop: 3,
                  color:
                    action.status === "success"
                      ? "var(--color-success)"
                      : action.status === "error"
                        ? "var(--color-error)"
                        : "var(--accent-ai)",
                }}
              >
                {action.status === "success" || action.status === "error"
                  ? "●"
                  : "◌"}
              </span>
              <div style={{ minWidth: 0, flex: 1 }}>
                <Text
                  style={{
                    display: "block",
                    fontSize: 12,
                    color:
                      action.status === "error"
                        ? "var(--color-error)"
                        : "var(--text-secondary)",
                    lineHeight: 1.5,
                    whiteSpace: "pre-wrap",
                  }}
                >
                  {action.detail || action.name}
                </Text>
                <Text
                  style={{
                    display: "block",
                    marginTop: 2,
                    fontSize: 11,
                    color: "var(--text-muted)",
                    lineHeight: 1.4,
                    whiteSpace: "pre-wrap",
                  }}
                >
                  {[
                    action.sourceLabel || action.source || "tool",
                    action.durationMs != null && action.durationMs > 0
                      ? `${action.durationMs}ms`
                      : "",
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </Text>
              </div>
            </div>
          ))}
        </div>
      )}
      {row.toolSummary && (
        <div style={{ marginTop: 6, display: "grid", gap: 6 }}>
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {`${t("operations.pmMetricTools", "工具调用")}: ${row.toolSummary.count} · ${t("operations.pmMetricErrors", "报错")}: ${row.toolSummary.errorCount}`}
          </Text>
          {row.toolSummary.byName.length > 0 && (
            <Space size={[6, 6]} wrap>
              {row.toolSummary.byName.map((item) => (
                <Tag
                  key={`seg-tool-count-${row.id}-${item.name}`}
                  style={{ marginRight: 0 }}
                >
                  {item.name} × {item.count}
                  {item.errorCount > 0
                    ? ` · ${t("operations.pmMetricErrors", "报错")} ${item.errorCount}`
                    : ""}
                </Tag>
              ))}
            </Space>
          )}
          {row.toolSummary.samples.length > 0 && (
            <div style={{ display: "grid", gap: 4 }}>
              {row.toolSummary.samples.map((sample) => (
                <div
                  key={`seg-tool-sample-${row.id}-${sample.idx}-${sample.tool}`}
                  style={{
                    border: "1px dashed var(--border-subtle)",
                    borderRadius: 8,
                    padding: "6px 8px",
                    background: "var(--bg-surface)",
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
                      color={sample.isError ? "error" : "default"}
                      style={{ marginRight: 0 }}
                    >
                      #{sample.idx} {sample.tool}
                    </Tag>
                    {sample.source && (
                      <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                        {sample.source}
                      </Text>
                    )}
                    {sample.durationMs != null && sample.durationMs > 0 && (
                      <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                        {sample.durationMs}ms
                      </Text>
                    )}
                  </div>
                  {sample.input && (
                    <Text
                      style={{
                        display: "block",
                        marginTop: 4,
                        fontSize: 11,
                        color: "var(--text-muted)",
                        whiteSpace: "pre-wrap",
                      }}
                    >
                      {`in: ${sample.input}`}
                    </Text>
                  )}
                  {sample.output && (
                    <Text
                      style={{
                        display: "block",
                        marginTop: 2,
                        fontSize: 11,
                        color: sample.isError
                          ? "var(--color-error)"
                          : "var(--text-muted)",
                        whiteSpace: "pre-wrap",
                      }}
                    >
                      {`out: ${sample.output}`}
                    </Text>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
      <PmSearchUsageBadges t={t} usage={row.searchUsage} />
      {row.excerpt.trim().length > 0 && (
        <Text
          style={{
            marginTop: 6,
            fontSize: 12,
            color: "var(--text-muted)",
            display: "block",
            whiteSpace: "pre-wrap",
          }}
        >
          {row.excerpt.trim().slice(-260)}
        </Text>
      )}
    </div>
  );
}

function PmSearchUsageBadges({
  t,
  usage,
}: {
  t: TFunction;
  usage: PmSearchUsageSummary | null;
}) {
  if (!usage || usage.rows.length === 0) return null;
  return (
    <div style={{ marginTop: 6 }}>
      <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
        {t("operations.pmSearchUsage", "联网调用")}
      </Text>
      <Space size={[4, 4]} wrap style={{ marginTop: 4 }}>
        {usage.rows.map((row) => (
          <Tag
            key={`search-usage-${row.layer}`}
            color={row.errorCount > 0 && row.successCount === 0 ? "warning" : "blue"}
            style={{ marginRight: 0 }}
          >
            {row.label || row.layer} × {row.attempts}
            {row.successCount > 0
              ? ` · ${t("operations.statusCompleted", "已完成")} ${row.successCount}`
              : ""}
            {row.errorCount > 0
              ? ` · ${t("operations.pmMetricErrors", "报错")} ${row.errorCount}`
              : ""}
            {(row.skippedCount ?? 0) > 0 ? ` · skipped ${row.skippedCount}` : ""}
          </Tag>
        ))}
      </Space>
    </div>
  );
}
