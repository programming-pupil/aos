import { agentApi, streamChatAdversarialRunEvents } from "@/api";
import { queryKeys } from "@/api/queryKeys";
import type { ChatAdversarialStreamEvent } from "@/types";
import type { TimelineMessage } from "@/pages/superAdversarial/types";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Collapse, Space, Tag, Typography } from "antd";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  buildRunTimeline,
  messageAccent,
} from "@/pages/superAdversarial/utils";
import { Markdown } from "./markdownRenderer";

const { Text } = Typography;
const HIDDEN_PROTOCOL_MARKERS = [
  "<aos_consensus_vote>",
  "<aos_evidence_request>",
] as const;

export function visibleAdversarialText(raw: string): string {
  const completeMarkerIndex = HIDDEN_PROTOCOL_MARKERS.reduce((earliest, marker) => {
    const index = raw.indexOf(marker);
    return index >= 0 && (earliest < 0 || index < earliest) ? index : earliest;
  }, -1);
  if (completeMarkerIndex >= 0) {
    return raw.slice(0, completeMarkerIndex).trimEnd();
  }
  for (const marker of HIDDEN_PROTOCOL_MARKERS) {
    for (let length = marker.length - 1; length > 0; length -= 1) {
      const prefix = marker.slice(0, length);
      if (raw.endsWith(prefix)) return raw.slice(0, -length).trimEnd();
    }
  }
  return raw;
}

function AdversarialTranscriptItem({
  item,
  modelErrorLabel,
}: {
  item: TimelineMessage;
  modelErrorLabel: string;
}) {
  const accent = messageAccent(item.role, item.model);
  const label = (
    <Space size={[6, 4]} wrap>
      <Text strong style={{ color: accent }}>
        {item.title}
      </Text>
      {item.subtitle ? <Tag>{item.subtitle}</Tag> : null}
      {item.error ? <Tag color="error">{modelErrorLabel}</Tag> : null}
    </Space>
  );

  return (
    <div
      style={{
        borderLeft: `3px solid ${accent}`,
        background: "var(--bg-surface)",
        borderRadius: 6,
        overflow: "hidden",
      }}
    >
      <Collapse
        ghost
        size="small"
        defaultActiveKey={["answer"]}
        items={[
          {
            key: "answer",
            label,
            children: (
              <div style={{ padding: "0 10px 8px" }}>
                <Markdown relaxed suppressHr>
                  {item.content}
                </Markdown>
              </div>
            ),
          },
        ]}
      />
    </div>
  );
}

function AdversarialAuditPanelImpl({
  runId,
  live = false,
}: {
  runId: string;
  live?: boolean;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [liveMessages, setLiveMessages] = useState<Record<string, TimelineMessage>>({});
  const liveRawTextRef = useRef<Record<string, string>>({});
  const latestSeqRef = useRef(0);
  const runQuery = useQuery({
    queryKey: queryKeys.chatAdversarial.detail(runId),
    queryFn: () => agentApi.getChatAdversarialRun(runId),
    enabled: Boolean(runId),
    staleTime: live ? 0 : Number.POSITIVE_INFINITY,
    refetchInterval: live ? 1500 : false,
  });

  const run = runQuery.data;
  const persistedTranscript = useMemo(
    () => run
      ? buildRunTimeline(run, t).filter(
          (item) => item.role === "model" || item.role === "judge",
        )
      : [],
    [run, t],
  );
  const transcript = useMemo(() => {
    const merged = new Map(persistedTranscript.map((item) => [item.id, item]));
    for (const item of Object.values(liveMessages)) {
      const persisted = merged.get(item.id);
      if (!persisted || item.typing || item.content.length >= persisted.content.length) {
        merged.set(item.id, persisted ? { ...persisted, ...item } : item);
      }
    }
    return Array.from(merged.values()).sort((left, right) => {
      const roundDiff = (left.round ?? 0) - (right.round ?? 0);
      if (roundDiff !== 0) return roundDiff;
      const roleOrder = { model: 0, judge: 1, final: 2, system: 3, user: 4 } as const;
      const roleDiff = roleOrder[left.role] - roleOrder[right.role];
      if (roleDiff !== 0) return roleDiff;
      return left.id.localeCompare(right.id);
    });
  }, [liveMessages, persistedTranscript]);
  const configuredModels = run?.models ?? [];
  const models = configuredModels.filter(
    (model, index, all) => model && all.indexOf(model) === index,
  );
  const isRunning =
    run && ["queued", "running", "cancelling"].includes(run.status);

  useEffect(() => {
    setLiveMessages({});
    liveRawTextRef.current = {};
    latestSeqRef.current = 0;
  }, [runId]);

  useEffect(() => {
    if (!runId || (!live && !isRunning)) return undefined;
    return streamChatAdversarialRunEvents(
      runId,
      {
        onEvent: (event: ChatAdversarialStreamEvent) => {
          if (event.seq > 0 && event.seq <= latestSeqRef.current) return;
          latestSeqRef.current = Math.max(latestSeqRef.current, event.seq ?? 0);
          if (["run_completed", "run_failed", "run_cancelled"].includes(event.event)) {
            void queryClient.invalidateQueries({
              queryKey: queryKeys.chatAdversarial.detail(runId),
            });
            return;
          }
          const role: TimelineMessage["role"] | null = event.event.startsWith("model_")
            ? "model"
            : event.event.startsWith("judge_")
              ? "judge"
              : null;
          if (!role || !event.messageId) return;
          const isDelta = event.event.endsWith("_delta");
          const isStarted = event.event.endsWith("_started");
          const isCompleted = event.event.endsWith("_completed");
          const isFailed = event.event.endsWith("_failed");
          const isCancelled = event.event.endsWith("_cancelled");
          const previousRaw = liveRawTextRef.current[event.messageId] ?? "";
          const nextRaw = isDelta
            ? `${previousRaw}${event.delta ?? ""}`
            : event.text ?? previousRaw;
          liveRawTextRef.current[event.messageId] = nextRaw;
          setLiveMessages((previous) => {
            const existing = previous[event.messageId];
            const model = event.model || existing?.model || t("chat.adversarialUnknownModel");
            const title = role === "judge"
              ? t("chat.adversarialJudgeWithModel", { model })
              : model;
            const content = visibleAdversarialText(nextRaw)
              || (isStarted ? "" : event.error || existing?.content || t("chat.adversarialNoTrace"));
            return {
              ...previous,
              [event.messageId]: {
                id: event.messageId,
                role,
                title,
                subtitle: role === "judge"
                  ? t("chat.adversarialRoundJudge", { round: event.round || "?" })
                  : t("chat.adversarialRoundSpeech", { round: event.round || "?" }),
                content,
                model,
                round: event.round ?? existing?.round,
                error: Boolean(event.error) || isFailed,
                typing: (isStarted || isDelta) && !isCompleted && !isFailed && !isCancelled,
              },
            };
          });
        },
        onError: (error) => {
          if (import.meta.env.DEV) {
            console.warn("[AdversarialAuditPanel] SSE failed; polling remains active:", error);
          }
        },
      },
      { afterSeq: latestSeqRef.current },
    );
  }, [isRunning, live, queryClient, runId, t]);

  const isCompleted = run?.status === "completed";

  const label = (
    <Space size={[6, 6]} wrap>
      <Text strong>{t("chat.adversarialAuditTitle", "多模型对抗记录")}</Text>
      {models.map((model) => (
        <Tag key={model} style={{ marginRight: 0 }}>
          {model}
        </Tag>
      ))}
      {run ? (
        <Tag color="blue" style={{ marginRight: 0 }}>
          {t("chat.adversarialCurrentRound", {
            current: run.current_round,
          })}
        </Tag>
      ) : null}
      {isCompleted && run?.winner_model ? (
        <Tag color="success" style={{ marginRight: 0 }}>
          {t("chat.adversarialWinnerWithModel", { model: run.winner_model })}
        </Tag>
      ) : null}
      {isRunning ? (
        <Tag color="processing">{t("operations.statusRunning", "运行中")}</Tag>
      ) : null}
    </Space>
  );

  return (
    <div
      style={{
        marginTop: 8,
        border: "1px solid var(--border-subtle)",
        borderRadius: 8,
        overflow: "hidden",
        background: "var(--bg-elevated)",
      }}
    >
      <Collapse
        key={runId}
        ghost
        size="small"
        items={[
          {
            key: "audit",
            label,
            children: runQuery.isLoading ? (
              <Text type="secondary">{t("common.loading", "加载中...")}</Text>
            ) : runQuery.isError ? (
              <Text type="danger">
                {t("chat.adversarialAuditLoadFailed", "对抗记录加载失败")}
              </Text>
            ) : (
              <div style={{ display: "grid", gap: 10 }}>
                {run?.judge_model ? (
                  <Text type="secondary">
                    {t("chat.adversarialJudgeWithModel", {
                      model: run.judge_model,
                    })}
                  </Text>
                ) : null}
                {transcript.length === 0 ? (
                  <Text type="secondary">
                    {isRunning
                      ? t("chat.adversarialRunningHint")
                      : t("chat.adversarialNoTrace")}
                  </Text>
                ) : (
                  transcript.map((item) => (
                    <AdversarialTranscriptItem
                      key={item.id}
                      item={item}
                      modelErrorLabel={t("chat.adversarialModelError")}
                    />
                  ))
                )}
                {isCompleted && run?.winner_reason ? (
                  <div
                    style={{
                      paddingTop: 8,
                      borderTop: "1px solid var(--border-subtle)",
                    }}
                  >
                    <Text strong>
                      {t("chat.adversarialWinnerReason", "胜出理由")}
                    </Text>
                    <Markdown relaxed suppressHr>
                      {run.winner_reason}
                    </Markdown>
                  </div>
                ) : null}
              </div>
            ),
          },
        ]}
      />
    </div>
  );
}

export const AdversarialAuditPanel = memo(AdversarialAuditPanelImpl);
