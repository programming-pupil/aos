import { Button, Card, Space, Tag, Tooltip, Typography } from "antd";
import type { TFunction } from "i18next";
import type {
  PmSubtaskAttemptRow,
  PmSubtaskRuntimeRow,
} from "@/api";
import type {
  PmClaimEvidence,
  PmConflictGraph,
  PmConflictRow,
  PmEvidenceTreeNode,
  PmQualitySnapshot,
  PmStageStatus,
  PmToolSummary,
} from "./chatCore.pmTypes";

const { Text } = Typography;

export interface PmResearchStatusStageView {
  id: string;
  label: string;
  status: PmStageStatus;
  attempt: number;
  durationMs: number | null;
  detail: string;
  rawDetail?: Record<string, unknown>;
  toolSummary: PmToolSummary | null;
}

export interface PmResearchStatusStageEventView {
  key: string;
  label: string;
  status: PmStageStatus;
  attempt: number;
  durationMs: number | null;
  detail: string;
  rawDetail?: Record<string, unknown>;
}

export function PmResearchStatusCard({
  t,
  progressPercent,
  backgroundTaskId,
  backgroundTaskStatus,
  stages,
  subtaskRows,
  subtaskAttempts,
  stageEvents,
  qualitySnapshot,
  isStreaming,
  evidenceTreeNodes,
  claimRows,
  conflictGraph,
  conflictRows,
  sourceUrls,
  onQuickFixBrowser,
  onQuickFixProxy,
  onQuickFixNarrow,
}: {
  t: TFunction;
  progressPercent: number;
  backgroundTaskId: string | null;
  backgroundTaskStatus: string | null;
  stages: PmResearchStatusStageView[];
  subtaskRows: PmSubtaskRuntimeRow[];
  subtaskAttempts: Record<string, PmSubtaskAttemptRow[]>;
  stageEvents: PmResearchStatusStageEventView[];
  qualitySnapshot: PmQualitySnapshot | null;
  isStreaming: boolean;
  evidenceTreeNodes: PmEvidenceTreeNode[];
  claimRows: PmClaimEvidence[];
  conflictGraph: PmConflictGraph | null;
  conflictRows: PmConflictRow[];
  sourceUrls: string[];
  onQuickFixBrowser: () => void;
  onQuickFixProxy: () => void;
  onQuickFixNarrow: () => void;
}) {
  return (
    <div
      style={{
        position: "sticky",
        top: 8,
        zIndex: 12,
      }}
    >
      <Card
        size="small"
        title={t("operations.pmResearchPanelTitle", "研究执行面板")}
        styles={{
          body: {
            padding: "12px 14px",
            maxHeight: "68vh",
            overflow: "auto",
          },
        }}
        style={{
          borderRadius: 12,
          borderColor: "var(--border-default)",
          background: "var(--bg-elevated)",
        }}
      >
        <PmStatusProgress
          t={t}
          progressPercent={progressPercent}
          backgroundTaskId={backgroundTaskId}
          backgroundTaskStatus={backgroundTaskStatus}
        />

        <PmStageGrid t={t} stages={stages} />

        <PmDeepLoopSection t={t} stages={stages} events={stageEvents} />

        <PmSubtaskSection
          t={t}
          rows={subtaskRows}
          attemptsBySubtask={subtaskAttempts}
        />

        <PmStageEventSection t={t} events={stageEvents} />

        <PmQualityGateSection
          t={t}
          qualitySnapshot={qualitySnapshot}
          isStreaming={isStreaming}
          onQuickFixBrowser={onQuickFixBrowser}
          onQuickFixProxy={onQuickFixProxy}
          onQuickFixNarrow={onQuickFixNarrow}
        />

        <PmEvidenceTreeSection t={t} nodes={evidenceTreeNodes} />

        <PmClaimAlignmentSection t={t} rows={claimRows} />

        <PmConflictGraphSection t={t} graph={conflictGraph} />

        <PmConflictMatrixSection t={t} rows={conflictRows} />

        <PmSourceLinksSection t={t} urls={isStreaming ? [] : sourceUrls} />
      </Card>
    </div>
  );
}

function stageStatusText(t: TFunction, status: PmStageStatus): string {
  if (status === "completed") return t("operations.statusCompleted", "已完成");
  if (status === "failed") return t("operations.statusFailed", "失败");
  if (status === "running") return t("operations.statusRunning", "运行中");
  return t("common.pending", "待处理");
}

function stageStatusColor(status: PmStageStatus): string {
  if (status === "completed") return "success";
  if (status === "failed") return "error";
  if (status === "running") return "processing";
  return "default";
}

function PmStatusProgress({
  t,
  progressPercent,
  backgroundTaskId,
  backgroundTaskStatus,
}: {
  t: TFunction;
  progressPercent: number;
  backgroundTaskId: string | null;
  backgroundTaskStatus: string | null;
}) {
  return (
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
        </Text>
        {backgroundTaskId && (
          <Space size={6} wrap style={{ marginTop: 4 }}>
            <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
              {t("operations.pmBackgroundTask", "后台任务")}
            </Text>
            <Tag color={backgroundStatusColor(backgroundTaskStatus)} style={{ marginRight: 0 }}>
              {backgroundStatusText(t, backgroundTaskStatus)}
            </Tag>
            <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
              {backgroundTaskId.slice(0, 18)}...
            </Text>
          </Space>
        )}
      </div>
    </div>
  );
}

function backgroundStatusColor(status: string | null): string {
  if (status === "completed") return "success";
  if (status === "failed" || status === "cancelled") return "error";
  if (status === "cancelling") return "warning";
  return "processing";
}

function backgroundStatusText(t: TFunction, status: string | null): string {
  if (status === "completed") return t("operations.statusCompleted", "已完成");
  if (status === "failed") return t("operations.statusFailed", "失败");
  if (status === "cancelled") {
    return t("operations.pmBackgroundCancelled", "已取消");
  }
  if (status === "cancelling") {
    return t("operations.pmBackgroundCancellingShort", "取消中");
  }
  return t("operations.statusRunning", "运行中");
}

function PmStageGrid({
  t,
  stages,
}: {
  t: TFunction;
  stages: PmResearchStatusStageView[];
}) {
  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "repeat(auto-fit, minmax(140px, 1fr))",
        gap: 8,
      }}
    >
      {stages.map((stage) => (
        <div
          key={stage.id}
          style={{
            border: "1px solid var(--border-subtle)",
            borderRadius: 10,
            padding: "8px 10px",
            background: "var(--bg-surface)",
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
            <Tag color={stageStatusColor(stage.status)} style={{ marginRight: 0 }}>
              {stageStatusText(t, stage.status)}
            </Tag>
          </div>
          <div
            style={{
              marginTop: 4,
              fontSize: 11,
              color: "var(--text-muted)",
              minHeight: 16,
            }}
          >
            {[
              stage.attempt > 1
                ? `${t("operations.retry", "重试")} #${stage.attempt}`
                : "",
              stage.durationMs != null && stage.durationMs > 0
                ? `${stage.durationMs}ms`
                : "",
            ]
              .filter(Boolean)
              .join(" · ")}
          </div>
          {stage.detail && (
            <Tooltip title={stage.detail}>
              <Text
                style={{
                  fontSize: 11,
                  color: "var(--text-muted)",
                  display: "block",
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
              >
                {stage.detail}
              </Text>
            </Tooltip>
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
                      key={`sticky-tool-${stage.id}-${row.name}`}
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
        </div>
      ))}
    </div>
  );
}

function PmSubtaskSection({
  t,
  rows,
  attemptsBySubtask,
}: {
  t: TFunction;
  rows: PmSubtaskRuntimeRow[];
  attemptsBySubtask: Record<string, PmSubtaskAttemptRow[]>;
}) {
  if (rows.length === 0) return null;

  return (
    <StatusSection title={t("operations.pmSubtaskRuntime", "子任务运行态")}>
      <div style={{ display: "grid", gap: 6 }}>
        {rows.slice(0, 12).map((row) => {
          const attempts = attemptsBySubtask[row.subtask_id || row.subtask_key] ?? [];
          const failedAttempts = attempts.filter(
            (attempt) =>
              attempt.status === "failed" || attempt.status === "timed_out",
          );
          const latestFailure =
            failedAttempts.length > 0
              ? failedAttempts[failedAttempts.length - 1]
              : null;
          const successRate =
            row.probe_candidate_count > 0
              ? Math.round(
                  (row.probe_completed_count / row.probe_candidate_count) * 100,
                )
              : 0;
          return (
            <div
              key={`pm-subtask-${row.subtask_key}`}
              style={{
                border: "1px solid var(--border-subtle)",
                borderRadius: 8,
                padding: "6px 8px",
                background: "var(--bg-surface)",
              }}
            >
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 8,
                }}
              >
                <Text style={{ fontSize: 12, color: "var(--text-primary)" }}>
                  {row.title}
                </Text>
                <Tag color={subtaskStatusColor(row.status)} style={{ marginRight: 0 }}>
                  {row.status}
                </Tag>
              </div>
              <Text
                style={{
                  fontSize: 11,
                  color: "var(--text-muted)",
                  display: "block",
                  marginTop: 2,
                }}
              >
                {`probe ${row.probe_completed_count}/${row.probe_candidate_count} · success ${successRate}% · cite ${row.citation_count} · domain ${row.domain_count}`}
              </Text>
              {latestFailure && (
                <Tooltip
                  title={latestFailure.error_message || latestFailure.error_code || ""}
                >
                  <Text
                    style={{
                      fontSize: 11,
                      color: "var(--text-muted)",
                      display: "block",
                    }}
                  >
                    {`failure: ${latestFailure.route_key || latestFailure.route_channel || "unknown"} · ${latestFailure.error_code || "runtime_error"}`}
                  </Text>
                </Tooltip>
              )}
            </div>
          );
        })}
      </div>
    </StatusSection>
  );
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function numberPercent(value: unknown): string | null {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return `${Math.round(value * 100)}%`;
}

function PmDeepLoopSection({
  t,
  stages,
  events,
}: {
  t: TFunction;
  stages: PmResearchStatusStageView[];
  events: PmResearchStatusStageEventView[];
}) {
  const latest =
    [...events]
      .reverse()
      .find((event) => event.label === "deep_loop" || event.rawDetail?.deepLoop)
      ?.rawDetail ??
    [...stages].reverse().find((stage) => stage.id === "deep_loop" || stage.rawDetail?.deepLoop)
      ?.rawDetail;
  const detail = asRecord(latest?.deepLoop) ?? asRecord(latest);
  if (!detail) return null;
  const decision = asRecord(detail.decision);
  const scores = asRecord(detail.scores);
  const evidenceScore = asRecord(detail.evidenceScore);
  const expertReview = asRecord(detail.expertReviewScore);
  const branchQueue = asRecord(detail.researchBranchQueue);
  const hypothesisGraph = asRecord(detail.hypothesisEvidenceGraph);
  const goldenHints = asRecord(detail.goldenEvalHints);
  const action = typeof decision?.action === "string" ? decision.action : "-";
  const reason = typeof decision?.reason === "string" ? decision.reason : "-";
  const loopState = typeof detail.loopState === "string" ? detail.loopState : "-";
  const readiness = numberPercent(scores?.decisionReadinessScore);
  const actionability = numberPercent(scores?.actionabilityScore);
  const firstParty = numberPercent(scores?.firstPartyAlignmentScore);
  const expertScore = numberPercent(expertReview?.overallScore);
  const preservationScore = numberPercent(expertReview?.evidencePreservationScore);
  const nonBlocking = expertReview?.nonBlocking === true;
  const conflict =
    typeof evidenceScore?.conflictLevel === "string"
      ? evidenceScore.conflictLevel
      : "-";
  const improvementAreas = Array.isArray(expertReview?.improvementAreas)
    ? expertReview.improvementAreas.filter((item): item is string => typeof item === "string")
    : [];
  const branchCount = Array.isArray(branchQueue?.branches) ? branchQueue.branches.length : 0;
  const unresolvedCount = Array.isArray(hypothesisGraph?.unresolvedNodeIds)
    ? hypothesisGraph.unresolvedNodeIds.length
    : 0;
  const goldenHintCount = Array.isArray(goldenHints?.hints)
    ? goldenHints.hints.filter(
        (item) => asRecord(item)?.satisfied === false,
      ).length
    : 0;

  return (
    <StatusSection title={t("operations.pmDeepLoopTitle", "Deep Research Loop")}>
      <Space size={[6, 6]} wrap>
        <Tag color="blue" style={{ marginRight: 0 }}>
          {loopState}
        </Tag>
        <Tag color={action === "finalize" ? "success" : action === "rewrite" ? "warning" : "processing"} style={{ marginRight: 0 }}>
          {action}
        </Tag>
        {readiness && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopReadiness", "就绪度")}: {readiness}
          </Text>
        )}
        {actionability && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopActionability", "可执行")}: {actionability}
          </Text>
        )}
        {firstParty && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopFirstParty", "一手对齐")}: {firstParty}
          </Text>
        )}
        {expertScore && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopExpertScore", "专家校准")}: {expertScore}
          </Text>
        )}
        {preservationScore && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopPreservation", "证据保留")}: {preservationScore}
          </Text>
        )}
        {nonBlocking && (
          <Tag color="default" style={{ marginRight: 0 }}>
            {t("operations.pmDeepLoopNonBlocking", "非阻断")}
          </Tag>
        )}
        {branchCount > 0 && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopBranches", "研究分支")}: {branchCount}
          </Text>
        )}
        {unresolvedCount > 0 && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopUnresolved", "未闭环")}: {unresolvedCount}
          </Text>
        )}
        {goldenHintCount > 0 && (
          <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
            {t("operations.pmDeepLoopEvalHints", "评测提示")}: {goldenHintCount}
          </Text>
        )}
        <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
          {t("operations.pmDeepLoopConflict", "冲突")}: {conflict}
        </Text>
      </Space>
      <Text
        style={{
          display: "block",
          marginTop: 6,
          fontSize: 12,
          color: "var(--text-secondary)",
        }}
      >
        {reason}
      </Text>
      {improvementAreas.length > 0 && (
        <Text
          style={{
            display: "block",
            marginTop: 4,
            fontSize: 12,
            color: "var(--text-muted)",
          }}
        >
          {t("operations.pmDeepLoopImprovements", "改进提示")}:{" "}
          {improvementAreas.slice(0, 2).join(" / ")}
        </Text>
      )}
    </StatusSection>
  );
}

function subtaskStatusColor(status: string): string {
  if (status === "completed") return "success";
  if (status === "failed") return "error";
  if (status === "running") return "processing";
  return "default";
}

function PmStageEventSection({
  t,
  events,
}: {
  t: TFunction;
  events: PmResearchStatusStageEventView[];
}) {
  if (events.length === 0) return null;

  return (
    <StatusSection title={t("operations.pmStageEventLog", "阶段事件流")}>
      <div style={{ display: "grid", gap: 4 }}>
        {events.map((event) => (
          <div
            key={event.key}
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 8,
              border: "1px solid var(--border-subtle)",
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
                minWidth: 0,
              }}
            >
              <Tag color={stageStatusColor(event.status)} style={{ marginRight: 0 }}>
                {stageStatusText(t, event.status)}
              </Tag>
              <Text style={{ fontSize: 12, color: "var(--text-secondary)" }}>
                {event.label}
              </Text>
              {event.attempt > 1 && (
                <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                  #{event.attempt}
                </Text>
              )}
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
              {event.durationMs != null && event.durationMs > 0 && (
                <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                  {event.durationMs}ms
                </Text>
              )}
              {event.detail && (
                <Tooltip title={event.detail}>
                  <Text
                    style={{
                      fontSize: 11,
                      color: "var(--text-muted)",
                      maxWidth: 260,
                      whiteSpace: "nowrap",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                    }}
                  >
                    {event.detail}
                  </Text>
                </Tooltip>
              )}
            </div>
          </div>
        ))}
      </div>
    </StatusSection>
  );
}

function PmQualityGateSection({
  t,
  qualitySnapshot,
  isStreaming,
  onQuickFixBrowser,
  onQuickFixProxy,
  onQuickFixNarrow,
}: {
  t: TFunction;
  qualitySnapshot: PmQualitySnapshot | null;
  isStreaming: boolean;
  onQuickFixBrowser: () => void;
  onQuickFixProxy: () => void;
  onQuickFixNarrow: () => void;
}) {
  if (!qualitySnapshot) return null;

  return (
    <StatusSection>
      <Space size={8} wrap>
        <Text style={{ fontSize: 12, color: "var(--text-secondary)" }}>
          {t("operations.qualityGateTitle", "质量门禁")}
        </Text>
        <Tag color={qualitySnapshot.passed ? "success" : "error"}>
          {qualitySnapshot.passed
            ? t("operations.statusCompleted", "已完成")
            : qualitySnapshot.deliverable
              ? t("operations.pmQualityPartial", "已降级可交付")
              : t("operations.pmQualityRepairing", "修复中")}
        </Tag>
        <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
          {t("operations.pmMetricTools", "工具")}: {qualitySnapshot.tool_call_count}
        </Text>
        <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
          {t("operations.pmMetricCitations", "引用")}:{" "}
          {qualitySnapshot.citation_count}
        </Text>
        <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
          {t("operations.pmMetricDomains", "域名")}:{" "}
          {qualitySnapshot.domain_count ?? 0}
        </Text>
        <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
          {t("operations.pmMetricClaims", "结论数")}:{" "}
          {qualitySnapshot.claim_count ?? 0}
        </Text>
        <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
          {t("operations.pmMetricAlignment", "对齐")}:{" "}
          {qualitySnapshot.claim_alignment_ok
            ? t("operations.pmMetricAlignmentOk", "达标")
            : t("operations.pmMetricAlignmentWeak", "偏弱")}
        </Text>
      </Space>
      {!qualitySnapshot.passed && qualitySnapshot.suggestions.length > 0 && (
        <div style={{ marginTop: 8 }}>
          {qualitySnapshot.suggestions.map((item, idx) => (
            <Text
              key={`${item}-${idx}`}
              style={{
                fontSize: 12,
                color: "var(--text-muted)",
                display: "block",
              }}
            >
              {idx + 1}. {item}
            </Text>
          ))}
        </div>
      )}
      {!qualitySnapshot.passed && !isStreaming && (
        <Space size={[8, 8]} wrap style={{ marginTop: 10 }}>
          <Button size="small" onClick={onQuickFixBrowser}>
            {t("operations.pmQuickFixBrowser", "一键切 Browser")}
          </Button>
          <Button size="small" onClick={onQuickFixProxy}>
            {t("operations.pmQuickFixProxy", "一键切代理")}
          </Button>
          <Button size="small" onClick={onQuickFixNarrow}>
            {t("operations.pmQuickFixNarrow", "一键缩小查询")}
          </Button>
        </Space>
      )}
    </StatusSection>
  );
}

function PmEvidenceTreeSection({
  t,
  nodes,
}: {
  t: TFunction;
  nodes: PmEvidenceTreeNode[];
}) {
  if (nodes.length === 0) return null;

  return (
    <StatusSection title={t("operations.pmEvidenceTree", "证据树")}>
      <div style={{ display: "grid", gap: 8 }}>
        {nodes.map((node, idx) => (
          <div
            key={`${idx}-${node.claim}`}
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
                gap: 6,
                flexWrap: "wrap",
              }}
            >
              <Tag
                color={node.status === "confirmed" ? "success" : "warning"}
                style={{ marginRight: 0 }}
              >
                {node.status === "confirmed"
                  ? t("operations.pmClaimCovered", "已覆盖")
                  : t("operations.pmClaimUncovered", "待补证")}
              </Tag>
              <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                {t("operations.pmEvidenceCount", "证据数")}: {node.evidence_count}
              </Text>
              <Tooltip title={node.claim}>
                <Text
                  style={{
                    fontSize: 12,
                    color: "var(--text-secondary)",
                    whiteSpace: "nowrap",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    maxWidth: 560,
                  }}
                >
                  {node.claim}
                </Text>
              </Tooltip>
            </div>
            <div
              style={{
                marginTop: 6,
                borderLeft: "2px solid var(--border-subtle)",
                paddingLeft: 8,
                display: "grid",
                gap: 6,
              }}
            >
              {node.evidences.slice(0, 4).map((leaf, leafIdx) => (
                <div key={`${idx}-leaf-${leafIdx}`}>
                  {leaf.url ? (
                    <a
                      href={leaf.url}
                      target="_blank"
                      rel="noreferrer"
                      style={{ fontSize: 11 }}
                    >
                      {leaf.domain || leaf.url}
                    </a>
                  ) : (
                    <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                      {t("operations.pmMissingEvidenceUrl", "缺少证据 URL")}
                    </Text>
                  )}
                  {leaf.excerpt && (
                    <Tooltip title={leaf.excerpt}>
                      <Text
                        style={{
                          display: "block",
                          fontSize: 11,
                          color: "var(--text-muted)",
                          whiteSpace: "nowrap",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          maxWidth: 560,
                        }}
                      >
                        {leaf.excerpt}
                      </Text>
                    </Tooltip>
                  )}
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </StatusSection>
  );
}

function PmClaimAlignmentSection({
  t,
  rows,
}: {
  t: TFunction;
  rows: PmClaimEvidence[];
}) {
  if (rows.length === 0) return null;

  return (
    <StatusSection title={t("operations.pmClaimEvidenceMap", "结论-证据对齐")}>
      <div style={{ display: "grid", gap: 6 }}>
        {rows.map((row, idx) => (
          <div
            key={`${idx}-${row.claim}`}
            style={{
              border: "1px solid var(--border-subtle)",
              borderRadius: 8,
              padding: "6px 8px",
              background: "var(--bg-surface)",
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
              <Tag color={row.cited ? "success" : "warning"} style={{ marginRight: 0 }}>
                {row.cited
                  ? t("operations.pmClaimCovered", "已覆盖")
                  : t("operations.pmClaimUncovered", "待补证")}
              </Tag>
              <Tooltip title={row.claim}>
                <Text
                  style={{
                    fontSize: 12,
                    color: "var(--text-secondary)",
                    whiteSpace: "nowrap",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    maxWidth: 520,
                  }}
                >
                  {row.claim}
                </Text>
              </Tooltip>
            </div>
            {row.evidence_excerpt && (
              <Tooltip title={row.evidence_excerpt}>
                <Text
                  style={{
                    display: "block",
                    marginTop: 4,
                    fontSize: 11,
                    color: "var(--text-muted)",
                    whiteSpace: "nowrap",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    maxWidth: 560,
                  }}
                >
                  {row.evidence_excerpt}
                </Text>
              </Tooltip>
            )}
            {row.urls.length > 0 && (
              <Space size={[6, 6]} wrap style={{ marginTop: 6 }}>
                {row.urls.slice(0, 3).map((url) => (
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
        ))}
      </div>
    </StatusSection>
  );
}

function PmConflictGraphSection({
  t,
  graph,
}: {
  t: TFunction;
  graph: PmConflictGraph | null;
}) {
  if (!graph || graph.edge_count <= 0) return null;

  return (
    <StatusSection title={t("operations.pmConflictGraph", "冲突裁决图")}>
      <Space size={[6, 6]} wrap>
        <Tag style={{ marginRight: 0 }}>
          {t("operations.pmConflictTopicCount", "主题")}: {graph.topic_count}
        </Tag>
        <Tag style={{ marginRight: 0 }}>
          {t("operations.pmConflictEdgeCount", "边")}: {graph.edge_count}
        </Tag>
        <Tag style={{ marginRight: 0 }}>
          {t("operations.pmConflictAdjudicated", "已裁决")}:{" "}
          {graph.adjudicated_count}
        </Tag>
        <Tag style={{ marginRight: 0 }}>
          {t("operations.pmConflictUnresolved", "未裁决")}:{" "}
          {graph.unresolved_count}
        </Tag>
        <Tag style={{ marginRight: 0 }}>
          {t("operations.pmConflictConfidence", "平均置信")}:{" "}
          {`${Math.round((graph.avg_confidence ?? 0) * 100)}%`}
        </Tag>
      </Space>
      <div style={{ marginTop: 8, display: "grid", gap: 6 }}>
        {graph.edges.slice(0, 6).map((edge, idx) => (
          <div
            key={`${idx}-${edge.topic}-${edge.relation}`}
            style={{
              border: "1px solid var(--border-subtle)",
              borderRadius: 8,
              padding: "6px 8px",
              background: "var(--bg-surface)",
            }}
          >
            <Space size={[6, 6]} wrap>
              <Tag color="processing" style={{ marginRight: 0 }}>
                {edge.topic || t("operations.pmConflictTopicFallback", "主题")}
              </Tag>
              <Tag color={conflictRelationColor(edge.relation)} style={{ marginRight: 0 }}>
                {conflictRelationText(t, edge.relation)}
              </Tag>
              <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                {`${t("operations.pmConflictConfidence", "置信")}: ${Math.round((edge.confidence ?? 0) * 100)}%`}
              </Text>
              <Text style={{ fontSize: 11, color: "var(--text-muted)" }}>
                {`${edge.source_left || "-"} ↔ ${edge.source_right || "-"}`}
              </Text>
            </Space>
          </div>
        ))}
      </div>
    </StatusSection>
  );
}

function conflictRelationColor(relation: string): string {
  if (relation === "contradicts") return "error";
  if (relation === "corroborates") return "success";
  return "warning";
}

function conflictRelationText(t: TFunction, relation: string): string {
  if (relation === "contradicts") {
    return t("operations.pmConflictRelationContradicts", "冲突");
  }
  if (relation === "corroborates") {
    return t("operations.pmConflictRelationCorroborates", "互证");
  }
  return t("operations.pmConflictRelationUnresolved", "待裁决");
}

function PmConflictMatrixSection({
  t,
  rows,
}: {
  t: TFunction;
  rows: PmConflictRow[];
}) {
  if (rows.length === 0) return null;

  return (
    <StatusSection title={t("operations.pmConflictMatrix", "冲突裁决矩阵")}>
      <div style={{ display: "grid", gap: 6 }}>
        {rows.map((row, idx) => (
          <div
            key={`${idx}-${row.topic}-${row.verdict}`}
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
              }}
            >
              <Tag color="processing" style={{ marginRight: 0 }}>
                {row.topic || t("operations.pmConflictTopicFallback", "主题")}
              </Tag>
              <Text style={{ fontSize: 12, color: "var(--text-secondary)" }}>
                {t("operations.pmConflictVerdict", "裁决")}: {row.verdict || "-"}
              </Text>
            </div>
            <div
              style={{
                display: "grid",
                gridTemplateColumns: "1fr 1fr",
                gap: 8,
              }}
            >
              <ConflictClaimBox label={`A · ${row.source_a || "-"}`} claim={row.claim_a} />
              <ConflictClaimBox label={`B · ${row.source_b || "-"}`} claim={row.claim_b} />
            </div>
          </div>
        ))}
      </div>
    </StatusSection>
  );
}

function ConflictClaimBox({
  label,
  claim,
}: {
  label: string;
  claim: string;
}) {
  return (
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
        {label}
      </Text>
      <Tooltip title={claim}>
        <Text
          style={{
            fontSize: 12,
            color: "var(--text-secondary)",
            display: "block",
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
          }}
        >
          {claim || "-"}
        </Text>
      </Tooltip>
    </div>
  );
}

function PmSourceLinksSection({
  t,
  urls,
}: {
  t: TFunction;
  urls: string[];
}) {
  if (urls.length === 0) return null;

  return (
    <StatusSection title={t("operations.pmEvidenceLinks", "证据来源")}>
      <Space size={[6, 6]} wrap>
        {urls.map((url) => (
          <a
            key={url}
            href={url}
            target="_blank"
            rel="noreferrer"
            style={{ fontSize: 12 }}
          >
            {url}
          </a>
        ))}
      </Space>
    </StatusSection>
  );
}

function StatusSection({
  title,
  children,
}: {
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <div
      style={{
        marginTop: 10,
        borderTop: "1px solid var(--border-subtle)",
        paddingTop: 10,
      }}
    >
      {title && (
        <Text
          style={{
            fontSize: 12,
            color: "var(--text-secondary)",
            marginBottom: 6,
            display: "block",
          }}
        >
          {title}
        </Text>
      )}
      {children}
    </div>
  );
}
