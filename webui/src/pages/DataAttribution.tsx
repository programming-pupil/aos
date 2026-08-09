import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Button,
  Card,
  Collapse,
  Drawer,
  Empty,
  Input,
  Popconfirm,
  Progress,
  Select,
  Space,
  Spin,
  Table,
  Tag,
  Typography,
  message,
} from "antd";
import {
  BarChartOutlined,
  CheckCircleOutlined,
  CloseCircleOutlined,
  DatabaseOutlined,
  DeleteOutlined,
  FileSearchOutlined,
  HistoryOutlined,
  InfoCircleOutlined,
  MessageOutlined,
  PlusOutlined,
  SearchOutlined,
} from "@ant-design/icons";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { dataSourcesApi, nl2sqlApi, streamNl2sqlAttributionTask } from "@/api";
import { queryKeys } from "@/api/queryKeys";
import type {
  AttributionAnalyzeResponse,
  AttributionConversationItem,
  AttributionDepth,
  AttributionObservation,
  AttributionTaskEvent,
  DataSourceInfo,
} from "@/types";

const { Text, Title, Paragraph } = Typography;
const ACTIVE_TASK_STORAGE_KEY = "aos:dataAttribution:activeTask";
const ACTIVE_TASK_MAX_AGE_MS = 24 * 60 * 60 * 1000;

interface StoredAttributionTask {
  taskId: string;
  question: string;
  depth: AttributionDepth;
  datasourceIds: string[];
  conversationId?: string | null;
  createdAt?: number;
}

interface AttributionRuntimeEvent {
  key: string;
  status: string;
  stage?: string | null;
  message: string;
  elapsedMs: number;
  progressPercent?: number | null;
  stepIndex?: number | null;
  stepTotal?: number | null;
  observation?: AttributionObservation | null;
  error?: string | null;
}

const STAGE_PERCENT: Record<string, number> = {
  queued: 5,
  understand: 14,
  plan: 26,
  execute: 62,
  diagnose: 84,
  synthesize: 88,
  completed: 100,
  clarification_needed: 100,
  failed: 100,
};

function isTerminalAttributionStatus(status?: string | null): boolean {
  return (
    status === "completed" ||
    status === "clarification_needed" ||
    status === "failed"
  );
}

function normalizeRuntimeEvent(input: {
  status: string;
  stage?: string | null;
  message?: string | null;
  elapsedMs: number;
  progressPercent?: number | null;
  stepIndex?: number | null;
  stepTotal?: number | null;
  observation?: AttributionObservation | null;
  error?: string | null;
}): AttributionRuntimeEvent {
  const message = input.message || input.error || input.status;
  return {
    key: [
      input.status,
      input.stage ?? "",
      message,
      input.progressPercent ?? "",
      input.stepIndex ?? "",
      input.stepTotal ?? "",
    ].join("|"),
    status: input.status,
    stage: input.stage,
    message,
    elapsedMs: input.elapsedMs,
    progressPercent: input.progressPercent,
    stepIndex: input.stepIndex,
    stepTotal: input.stepTotal,
    observation: input.observation,
    error: input.error,
  };
}

function formatMs(ms?: number | null): string {
  if (!ms || ms <= 0) return "-";
  if (ms < 1000) return `${ms}ms`;
  return `${Math.round(ms / 1000)}s`;
}

function stringifyCell(value: unknown): string {
  if (value == null) return "";
  if (
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  ) {
    return String(value);
  }
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function EvidenceTable({
  observation,
}: {
  observation: AttributionObservation;
}) {
  const columns = (observation.columns ?? []).slice(0, 8).map((col) => ({
    title: col,
    dataIndex: col,
    key: col,
    ellipsis: true,
    render: (value: unknown) => (
      <Text style={{ fontSize: 12 }}>{stringifyCell(value)}</Text>
    ),
  }));
  const rows = (observation.rows ?? [])
    .slice(0, 8)
    .map((row, idx) => ({ key: idx, ...row }));

  if (!columns.length || !rows.length) {
    return (
      <Empty
        image={Empty.PRESENTED_IMAGE_SIMPLE}
        description="暂无可展示数据"
      />
    );
  }

  return (
    <Table
      size="small"
      columns={columns}
      dataSource={rows}
      pagination={false}
      scroll={{ x: true }}
    />
  );
}

function RuntimeObservationDetails({
  observation,
}: {
  observation: AttributionObservation;
}) {
  const { t } = useTranslation();
  const items = [];

  if (observation.error) {
    items.push({
      key: "error",
      label: t("dataAttribution.failureReason"),
      children: <Alert type="error" showIcon message={observation.error} />,
    });
  }

  items.push({
    key: "sql",
    label: `${t("dataAttribution.sqlDetail")} (${observation.sqls?.length ?? 0})`,
    children: observation.sqls?.length ? (
      <Space direction="vertical" size={8} style={{ width: "100%" }}>
        {observation.sqls.map((sql, idx) => (
          <pre
            key={`${observation.stepId}-sql-${idx}`}
            style={{
              margin: 0,
              padding: 10,
              borderRadius: 6,
              overflow: "auto",
              maxWidth: "100%",
              maxHeight: 260,
              background: "rgba(0,0,0,0.24)",
              border: "1px solid var(--border-subtle)",
              fontSize: 12,
              lineHeight: 1.55,
              whiteSpace: "pre-wrap",
              overflowWrap: "anywhere",
            }}
          >
            {sql}
          </pre>
        ))}
      </Space>
    ) : (
      <Empty
        image={Empty.PRESENTED_IMAGE_SIMPLE}
        description={t("dataAttribution.noSqlGenerated")}
      />
    ),
  });

  items.push({
    key: "result",
    label: t("dataAttribution.queryResult"),
    children: observation.rows?.length ? (
      <Space direction="vertical" size={8} style={{ width: "100%" }}>
        <Space wrap>
          <Tag>{observation.rowCount} rows</Tag>
          <Tag>{formatMs(observation.elapsedMs)}</Tag>
          {observation.sampled ? (
            <Tag color="blue">{t("dataAttribution.sampled")}</Tag>
          ) : null}
        </Space>
        <EvidenceTable observation={observation} />
      </Space>
    ) : (
      <Empty
        image={Empty.PRESENTED_IMAGE_SIMPLE}
        description={t("dataAttribution.noQueryRows")}
      />
    ),
  });

  return <Collapse size="small" ghost items={items} />;
}

export default function DataAttribution() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [question, setQuestion] = useState("");
  const [depth, setDepth] = useState<AttributionDepth>("standard");
  const [selectedDatasourceIds, setSelectedDatasourceIds] = useState<string[]>(
    [],
  );
  const [currentConversationId, setCurrentConversationId] = useState<
    string | null
  >(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [loadingConversationId, setLoadingConversationId] = useState<
    string | null
  >(null);
  const [currentTaskId, setCurrentTaskId] = useState<string | null>(null);
  const [stage, setStage] = useState<string | null>(null);
  const [stageMessage, setStageMessage] = useState<string | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [progressPercent, setProgressPercent] = useState<number | null>(null);
  const [runtimeEvents, setRuntimeEvents] = useState<AttributionRuntimeEvent[]>(
    [],
  );
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<AttributionAnalyzeResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const unsubscribeRef = useRef<null | (() => void)>(null);

  const { data: dataSourcesData } = useQuery({
    queryKey: ["dataSources", "attribution"],
    queryFn: () => dataSourcesApi.list({ page: 1, per_page: 200 }),
    retry: false,
  });

  const {
    data: attributionConversations,
    isLoading: historyLoading,
    refetch: refetchAttributionHistory,
  } = useQuery({
    queryKey: queryKeys.nl2sql.attributionConversations.list({
      page: 1,
      per_page: 50,
    }),
    queryFn: () =>
      nl2sqlApi.listAttributionConversations({ page: 1, per_page: 50 }),
    enabled: historyOpen,
    staleTime: 20_000,
  });

  const dataSourceOptions = useMemo(
    () =>
      (dataSourcesData?.data_sources ?? [])
        .filter((ds: DataSourceInfo) => ds.enabled)
        .map((ds) => ({
          value: ds.id,
          label: `${ds.name} · ${ds.db_type}`,
        })),
    [dataSourcesData],
  );

  const persistTask = useCallback((payload: StoredAttributionTask | null) => {
    if (!payload) {
      localStorage.removeItem(ACTIVE_TASK_STORAGE_KEY);
      return;
    }
    localStorage.setItem(ACTIVE_TASK_STORAGE_KEY, JSON.stringify(payload));
  }, []);

  const stageLabel = useCallback(
    (value?: string | null) => {
      switch (value) {
        case "queued":
          return t("dataAttribution.stageQueuedShort");
        case "understand":
          return t("dataAttribution.stageUnderstand");
        case "plan":
          return t("dataAttribution.stagePlan");
        case "execute":
          return t("dataAttribution.stageExecute");
        case "diagnose":
          return t("dataAttribution.stageDiagnose");
        case "synthesize":
          return t("dataAttribution.stageSynthesize");
        case "completed":
          return t("dataAttribution.stageCompleted");
        case "partial":
          return t("dataAttribution.status_partial");
        case "no_data":
          return t("dataAttribution.status_no_data");
        case "clarification_needed":
          return t("dataAttribution.stageClarification");
        case "failed":
          return t("dataAttribution.stageFailed");
        default:
          return value || t("dataAttribution.stageIdle");
      }
    },
    [t],
  );

  const appendRuntimeEvent = useCallback((event: AttributionRuntimeEvent) => {
    setRuntimeEvents((prev) => {
      if (prev.some((item) => item.key === event.key)) return prev;
      return [...prev, event].slice(-40);
    });
  }, []);

  const attachTaskStream = useCallback(
    (taskId: string) => {
      unsubscribeRef.current?.();
      unsubscribeRef.current = streamNl2sqlAttributionTask(taskId, {
        onEvent: (evt: AttributionTaskEvent) => {
          setStage(evt.stage ?? evt.status);
          setStageMessage(evt.message ?? null);
          setElapsedMs(evt.elapsed_ms ?? 0);
          if (typeof evt.progress_percent === "number") {
            setProgressPercent(evt.progress_percent);
          }
          appendRuntimeEvent(
            normalizeRuntimeEvent({
              status: evt.status,
              stage: evt.stage,
              message: evt.message,
              elapsedMs: evt.elapsed_ms,
              progressPercent: evt.progress_percent,
              stepIndex: evt.step_index,
              stepTotal: evt.step_total,
              observation: evt.observation,
              error: evt.error,
            }),
          );
          if (evt.response) {
            setResult(evt.response);
            setCurrentConversationId(evt.response.conversationId ?? null);
          }
          if (evt.error) {
            setError(evt.error);
          }
        },
        onDone: (evt) => {
          setRunning(false);
          setStage(evt.stage ?? evt.status);
          setStageMessage(evt.message ?? null);
          if (typeof evt.progress_percent === "number") {
            setProgressPercent(evt.progress_percent);
          }
          persistTask(null);
          if (evt.response) {
            setResult(evt.response);
            setCurrentConversationId(evt.response.conversationId ?? null);
          }
          if (evt.error) {
            setError(evt.error);
          }
          queryClient.invalidateQueries({
            queryKey: queryKeys.nl2sql.attributionConversations.all,
          });
        },
        onError: (msg) => {
          setRunning(false);
          setError(msg);
          setProgressPercent(100);
          appendRuntimeEvent(
            normalizeRuntimeEvent({
              status: "failed",
              stage: "failed",
              message: msg,
              elapsedMs: 0,
              progressPercent: 100,
              error: msg,
            }),
          );
          message.error(msg);
        },
      });
    },
    [appendRuntimeEvent, persistTask, queryClient],
  );

  useEffect(() => {
    let cancelled = false;
    const raw = localStorage.getItem(ACTIVE_TASK_STORAGE_KEY);
    if (!raw) {
      return () => {
        unsubscribeRef.current?.();
        unsubscribeRef.current = null;
      };
    }

    try {
      const stored = JSON.parse(raw) as StoredAttributionTask;
      if (!stored?.taskId) throw new Error("invalid task");
      if (
        stored.createdAt &&
        Date.now() - stored.createdAt > ACTIVE_TASK_MAX_AGE_MS
      ) {
        throw new Error("expired task");
      }
      setCurrentTaskId(stored.taskId);
      setCurrentConversationId(stored.conversationId ?? null);
      setQuestion(stored.question ?? "");
      setDepth(stored.depth ?? "standard");
      setSelectedDatasourceIds(stored.datasourceIds ?? []);
      nl2sqlApi
        .getAttributionTaskStatus(stored.taskId)
        .then((status) => {
          if (cancelled) return;
          setStage(status.stage ?? status.status);
          setStageMessage(status.message ?? null);
          setElapsedMs(status.elapsedMs ?? 0);
          if (typeof status.progressPercent === "number") {
            setProgressPercent(status.progressPercent);
          }
          appendRuntimeEvent(
            normalizeRuntimeEvent({
              status: status.status,
              stage: status.stage,
              message: status.message,
              elapsedMs: status.elapsedMs,
              progressPercent: status.progressPercent,
              stepIndex: status.stepIndex,
              stepTotal: status.stepTotal,
              observation: status.observation,
              error: status.error,
            }),
          );
          if (status.response) {
            setResult(status.response);
            setCurrentConversationId(
              status.response.conversationId ?? stored.conversationId ?? null,
            );
          }
          if (status.error) {
            setError(status.error);
          }
          const terminal = isTerminalAttributionStatus(status.status);
          setRunning(!terminal);
          if (!terminal) {
            attachTaskStream(stored.taskId);
          } else {
            persistTask(null);
          }
        })
        .catch(() => {
          if (!cancelled) {
            persistTask(null);
            setCurrentTaskId(null);
            setCurrentConversationId(null);
            setRunning(false);
            setProgressPercent(null);
          }
        });
    } catch {
      persistTask(null);
      setCurrentTaskId(null);
      setCurrentConversationId(null);
      setRunning(false);
      setProgressPercent(null);
    }

    return () => {
      cancelled = true;
      unsubscribeRef.current?.();
      unsubscribeRef.current = null;
    };
  }, [appendRuntimeEvent, attachTaskStream, persistTask]);

  const handleRun = async () => {
    const trimmed = question.trim();
    if (!trimmed) {
      message.warning(t("dataAttribution.emptyQuestion"));
      return;
    }

    unsubscribeRef.current?.();
    unsubscribeRef.current = null;
    persistTask(null);
    setCurrentTaskId(null);
    setRunning(true);
    setError(null);
    setResult(null);
    setStage("queued");
    setStageMessage(t("dataAttribution.stageQueued"));
    setProgressPercent(3);
    setElapsedMs(0);
    setRuntimeEvents([
      normalizeRuntimeEvent({
        status: "queued",
        stage: "queued",
        message: t("dataAttribution.stageQueued"),
        elapsedMs: 0,
        progressPercent: 3,
      }),
    ]);

    try {
      const start = await nl2sqlApi.attributionAnalyzeAsync({
        question: trimmed,
        depth,
        datasource_ids: selectedDatasourceIds,
        conversation_id: currentConversationId ?? undefined,
      });
      setCurrentTaskId(start.taskId);
      setCurrentConversationId(start.conversationId);
      persistTask({
        taskId: start.taskId,
        question: trimmed,
        depth,
        datasourceIds: selectedDatasourceIds,
        conversationId: start.conversationId,
        createdAt: Date.now(),
      });
      attachTaskStream(start.taskId);
    } catch (e) {
      const msg = (e as Error).message;
      setRunning(false);
      setError(msg);
      setProgressPercent(100);
      appendRuntimeEvent(
        normalizeRuntimeEvent({
          status: "failed",
          stage: "failed",
          message: msg,
          elapsedMs,
          progressPercent: 100,
          error: msg,
        }),
      );
      message.error(msg);
    }
  };

  const handleNewConversation = () => {
    unsubscribeRef.current?.();
    unsubscribeRef.current = null;
    persistTask(null);
    setQuestion("");
    setCurrentConversationId(null);
    setCurrentTaskId(null);
    setStage(null);
    setStageMessage(null);
    setElapsedMs(0);
    setProgressPercent(null);
    setRuntimeEvents([]);
    setRunning(false);
    setResult(null);
    setError(null);
  };

  const handleLoadConversation = async (conversationId: string) => {
    setLoadingConversationId(conversationId);
    try {
      const detail = await nl2sqlApi.getAttributionConversation(conversationId);
      const latest = detail.tasks[detail.tasks.length - 1] ?? null;
      setCurrentConversationId(detail.id);
      setCurrentTaskId(latest?.taskId ?? null);
      setQuestion("");
      setError(latest?.error ?? null);
      setRunning(false);
      setRuntimeEvents([]);
      setProgressPercent(latest ? 100 : null);
      setElapsedMs(latest?.totalExecutionMs ?? 0);
      setStage(latest?.status ?? null);
      setStageMessage(latest ? t("dataAttribution.historyLoaded") : null);
      setResult(latest?.response ?? null);
      setHistoryOpen(false);
      persistTask(null);
    } catch (e) {
      message.error((e as Error).message);
    } finally {
      setLoadingConversationId(null);
    }
  };

  const handleDeleteConversation = async (conversationId: string) => {
    try {
      await nl2sqlApi.deleteAttributionConversation(conversationId);
      if (currentConversationId === conversationId) {
        handleNewConversation();
      }
      await refetchAttributionHistory();
    } catch (e) {
      message.error((e as Error).message);
    }
  };

  const report = result?.report ?? null;
  const observations = result?.observations ?? [];
  const evidenceHealth = result?.evidenceHealth ?? null;
  const resultStatus = result?.status ?? null;
  const terminalProblem = Boolean(error) || resultStatus === "no_data";
  const progress =
    progressPercent ?? STAGE_PERCENT[stage ?? "queued"] ?? (running ? 40 : 0);
  const progressIsRealtime = progressPercent != null;

  return (
    <div
      style={{
        minHeight: "100%",
        width: "100%",
        maxWidth: "100%",
        boxSizing: "border-box",
        overflowX: "hidden",
        background: "var(--bg-void)",
        padding: 20,
      }}
    >
      <div
        style={{
          maxWidth: 1280,
          width: "100%",
          minWidth: 0,
          margin: "0 auto",
          display: "grid",
          gap: 16,
        }}
      >
        <section
          style={{
            border: "1px solid var(--border-subtle)",
            background: "var(--bg-surface)",
            borderRadius: 8,
            padding: 16,
            boxShadow: "var(--shadow-card)",
            minWidth: 0,
          }}
        >
          <Space
            align="center"
            wrap
            style={{
              width: "100%",
              justifyContent: "space-between",
              marginBottom: 12,
              minWidth: 0,
            }}
          >
            <Space wrap style={{ minWidth: 0 }}>
              <BarChartOutlined style={{ color: "#1677ff" }} />
              <Title level={4} style={{ margin: 0 }}>
                {t("dataAttribution.title")}
              </Title>
              {currentConversationId ? (
                <Tag color="blue">{t("dataAttribution.inConversation")}</Tag>
              ) : null}
            </Space>
            <Space wrap>
              {currentTaskId ? (
                <Tag color={running ? "processing" : "default"}>
                  {running
                    ? t("dataAttribution.runningTask")
                    : t("dataAttribution.lastResult")}
                </Tag>
              ) : null}
              <Button
                icon={<HistoryOutlined />}
                onClick={() => setHistoryOpen(true)}
              >
                {t("dataAttribution.history")}
              </Button>
              <Button
                icon={<PlusOutlined />}
                onClick={handleNewConversation}
                disabled={running}
              >
                {t("dataAttribution.newConversation")}
              </Button>
            </Space>
          </Space>

          <Input.TextArea
            value={question}
            onChange={(e) => setQuestion(e.target.value)}
            placeholder={t("dataAttribution.placeholder")}
            autoSize={{ minRows: 3, maxRows: 8 }}
            disabled={running}
            style={{ marginBottom: 12, maxWidth: "100%" }}
          />

          <div
            style={{
              display: "flex",
              gap: 10,
              flexWrap: "wrap",
              alignItems: "center",
              minWidth: 0,
            }}
          >
            <Select
              value={depth}
              onChange={setDepth}
              disabled={running}
              style={{ width: 140 }}
              options={[
                { value: "fast", label: t("dataAttribution.depthFast") },
                {
                  value: "standard",
                  label: t("dataAttribution.depthStandard"),
                },
                { value: "deep", label: t("dataAttribution.depthDeep") },
              ]}
            />
            <Select
              mode="multiple"
              allowClear
              maxTagCount="responsive"
              value={selectedDatasourceIds}
              onChange={setSelectedDatasourceIds}
              disabled={running}
              style={{ minWidth: 0, flex: "1 1 260px" }}
              placeholder={t("dataAttribution.datasourceAuto")}
              options={dataSourceOptions}
            />
            <Button
              type="primary"
              icon={<SearchOutlined />}
              loading={running}
              onClick={handleRun}
            >
              {currentConversationId && result
                ? t("dataAttribution.followupRun")
                : t("dataAttribution.run")}
            </Button>
          </div>
        </section>

        {(running || stage || error) && (
          <section
            style={{
              border: "1px solid var(--border-subtle)",
              background: "var(--bg-surface)",
              borderRadius: 8,
              padding: 16,
              minWidth: 0,
              overflowX: "hidden",
            }}
          >
            <Space
              align="center"
              wrap
              style={{
                width: "100%",
                justifyContent: "space-between",
                minWidth: 0,
              }}
            >
              <Space wrap style={{ minWidth: 0 }}>
                {running ? (
                  <Spin size="small" />
                ) : terminalProblem ? (
                  <CloseCircleOutlined style={{ color: "#ff4d4f" }} />
                ) : (
                  <CheckCircleOutlined style={{ color: "#52c41a" }} />
                )}
                <Text strong style={{ overflowWrap: "anywhere" }}>
                  {stageMessage ?? t("dataAttribution.stageIdle")}
                </Text>
              </Space>
              <Text type="secondary">{formatMs(elapsedMs)}</Text>
            </Space>
            <Progress
              percent={progress}
              size="small"
              status={
                terminalProblem ? "exception" : running ? "active" : "success"
              }
            />
            <Space size={8} wrap style={{ marginTop: 8 }}>
              <Tag color={progressIsRealtime ? "blue" : "default"}>
                {progressIsRealtime
                  ? t("dataAttribution.progressRealtime")
                  : t("dataAttribution.progressEstimated")}
              </Tag>
              {runtimeEvents[runtimeEvents.length - 1]?.stepIndex &&
              runtimeEvents[runtimeEvents.length - 1]?.stepTotal ? (
                <Tag>
                  {t("dataAttribution.stepProgress", {
                    current: runtimeEvents[runtimeEvents.length - 1]?.stepIndex,
                    total: runtimeEvents[runtimeEvents.length - 1]?.stepTotal,
                  })}
                </Tag>
              ) : null}
            </Space>
            {!!runtimeEvents.length && (
              <div style={{ marginTop: 14 }}>
                <Text strong>{t("dataAttribution.processDetails")}</Text>
                <div style={{ marginTop: 8, display: "grid", gap: 8 }}>
                  {runtimeEvents.slice(-10).map((evt, idx, arr) => {
                    const isLatest = idx === arr.length - 1;
                    const isFailed =
                      evt.status === "failed" || Boolean(evt.error);
                    const isDone =
                      isTerminalAttributionStatus(evt.status) && !isFailed;
                    return (
                      <div key={evt.key}>
                        <div
                          style={{
                            display: "grid",
                            gridTemplateColumns: "10px minmax(0, 1fr) auto",
                            gap: 10,
                            alignItems: "center",
                            color: isLatest
                              ? "inherit"
                              : "var(--text-secondary, #8c8c8c)",
                            minWidth: 0,
                          }}
                        >
                          <span
                            style={{
                              width: 8,
                              height: 8,
                              borderRadius: "50%",
                              background: isFailed
                                ? "#ff4d4f"
                                : isDone
                                  ? "#52c41a"
                                  : isLatest
                                    ? "#1677ff"
                                    : "var(--border-strong, #8c8c8c)",
                            }}
                          />
                          <Space size={8} wrap style={{ minWidth: 0 }}>
                            <Tag
                              color={
                                isFailed
                                  ? "red"
                                  : isDone
                                    ? "green"
                                    : isLatest
                                      ? "processing"
                                      : "default"
                              }
                            >
                              {stageLabel(evt.stage ?? evt.status)}
                            </Tag>
                            <Text style={{ overflowWrap: "anywhere" }}>
                              {evt.message}
                            </Text>
                          </Space>
                          <Text type="secondary">
                            {formatMs(evt.elapsedMs)}
                          </Text>
                        </div>
                        {evt.observation ? (
                          <div style={{ marginLeft: 20, marginTop: 8 }}>
                            <RuntimeObservationDetails
                              observation={evt.observation}
                            />
                          </div>
                        ) : null}
                      </div>
                    );
                  })}
                </div>
              </div>
            )}
            {error ? (
              <Alert
                type="error"
                showIcon
                message={error}
                style={{ marginTop: 10 }}
              />
            ) : null}
          </section>
        )}

        {result?.clarificationQuestion ? (
          <Alert
            type="warning"
            showIcon
            icon={<InfoCircleOutlined />}
            message={t("dataAttribution.needClarification")}
            description={result.clarificationQuestion}
          />
        ) : null}

        {report ? (
          <section
            style={{
              border: "1px solid var(--border-subtle)",
              background: "var(--bg-surface)",
              borderRadius: 8,
              padding: 18,
              minWidth: 0,
              overflowX: "hidden",
            }}
          >
            <Space direction="vertical" size={14} style={{ width: "100%" }}>
              <div>
                <Title level={3} style={{ marginTop: 0, marginBottom: 8 }}>
                  {report.title}
                </Title>
                <Paragraph
                  style={{
                    fontSize: 15,
                    lineHeight: 1.8,
                    marginBottom: 0,
                    overflowWrap: "anywhere",
                  }}
                >
                  {report.executiveSummary}
                </Paragraph>
              </div>

              {report.metricAnswer ? (
                <Alert
                  type="info"
                  showIcon
                  message={t("dataAttribution.metricAnswer")}
                  description={report.metricAnswer}
                />
              ) : null}

              {evidenceHealth ? (
                <div
                  style={{
                    display: "grid",
                    gridTemplateColumns: "repeat(auto-fit, minmax(150px, 1fr))",
                    gap: 10,
                  }}
                >
                  <div
                    style={{
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 8,
                      padding: 10,
                    }}
                  >
                    <Text type="secondary">
                      {t("dataAttribution.totalSteps")}
                    </Text>
                    <div style={{ fontSize: 22, fontWeight: 700 }}>
                      {evidenceHealth.totalSteps}
                    </div>
                  </div>
                  <div
                    style={{
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 8,
                      padding: 10,
                    }}
                  >
                    <Text type="secondary">
                      {t("dataAttribution.successfulSteps")}
                    </Text>
                    <div
                      style={{
                        fontSize: 22,
                        fontWeight: 700,
                        color: "#52c41a",
                      }}
                    >
                      {evidenceHealth.successfulSteps}
                    </div>
                  </div>
                  <div
                    style={{
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 8,
                      padding: 10,
                    }}
                  >
                    <Text type="secondary">
                      {t("dataAttribution.failedSteps")}
                    </Text>
                    <div
                      style={{
                        fontSize: 22,
                        fontWeight: 700,
                        color:
                          evidenceHealth.failedSteps > 0
                            ? "#ff4d4f"
                            : "inherit",
                      }}
                    >
                      {evidenceHealth.failedSteps}
                    </div>
                  </div>
                  <div
                    style={{
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 8,
                      padding: 10,
                    }}
                  >
                    <Text type="secondary">
                      {t("dataAttribution.totalRows")}
                    </Text>
                    <div style={{ fontSize: 22, fontWeight: 700 }}>
                      {evidenceHealth.totalRows}
                    </div>
                  </div>
                </div>
              ) : null}

              {!!report.mainCauses?.length && (
                <div>
                  <Title level={5}>{t("dataAttribution.mainCauses")}</Title>
                  <div style={{ display: "grid", gap: 10 }}>
                    {report.mainCauses.map((cause, idx) => (
                      <div
                        key={`${cause.title}-${idx}`}
                        style={{
                          border: "1px solid var(--border-subtle)",
                          borderRadius: 8,
                          padding: 12,
                          background: "rgba(255,255,255,0.03)",
                        }}
                      >
                        <Space style={{ marginBottom: 6 }}>
                          <Tag color="blue">{idx + 1}</Tag>
                          <Text strong>{cause.title}</Text>
                          {cause.confidence ? (
                            <Tag>{cause.confidence}</Tag>
                          ) : null}
                        </Space>
                        <Paragraph
                          style={{ marginBottom: 6, overflowWrap: "anywhere" }}
                        >
                          {cause.explanation}
                        </Paragraph>
                        {cause.impact ? (
                          <Text
                            type="secondary"
                            style={{ overflowWrap: "anywhere" }}
                          >
                            {cause.impact}
                          </Text>
                        ) : null}
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {!!report.recommendations?.length && (
                <div>
                  <Title level={5}>
                    {t("dataAttribution.recommendations")}
                  </Title>
                  <ul style={{ margin: 0, paddingLeft: 20 }}>
                    {report.recommendations.map((item, idx) => (
                      <li key={idx}>{item}</li>
                    ))}
                  </ul>
                </div>
              )}

              {!!report.nextQuestions?.length && (
                <div>
                  <Title level={5}>{t("dataAttribution.nextQuestions")}</Title>
                  <ul style={{ margin: 0, paddingLeft: 20 }}>
                    {report.nextQuestions.map((item, idx) => (
                      <li key={idx}>{item}</li>
                    ))}
                  </ul>
                </div>
              )}

              <Space wrap>
                {resultStatus ? (
                  <Tag
                    color={
                      resultStatus === "completed"
                        ? "green"
                        : resultStatus === "partial"
                          ? "orange"
                          : "red"
                    }
                  >
                    {t("dataAttribution.status")}:{" "}
                    {t(`dataAttribution.status_${resultStatus}`, resultStatus)}
                  </Tag>
                ) : null}
                {report.confidence ? (
                  <Tag color="green">
                    {t("dataAttribution.confidence")}: {report.confidence}
                  </Tag>
                ) : null}
                {report.coverage ? (
                  <Tag color="geekblue">{report.coverage}</Tag>
                ) : null}
                <Tag>
                  {t("dataAttribution.elapsed")}:{" "}
                  {formatMs(result?.totalExecutionMs)}
                </Tag>
              </Space>

              {!!report.caveats?.length && (
                <Alert
                  type="warning"
                  showIcon
                  message={t("dataAttribution.caveats")}
                  description={
                    <ul style={{ margin: 0, paddingLeft: 18 }}>
                      {report.caveats.map((item, idx) => (
                        <li key={idx}>{item}</li>
                      ))}
                    </ul>
                  }
                />
              )}
            </Space>
          </section>
        ) : !running && !result ? (
          <section
            style={{
              border: "1px dashed var(--border-subtle)",
              background: "var(--bg-surface)",
              borderRadius: 8,
              padding: 36,
              minWidth: 0,
            }}
          >
            <Empty description={t("dataAttribution.emptyState")} />
          </section>
        ) : null}

        {!!observations.length && (
          <section
            style={{
              border: "1px solid var(--border-subtle)",
              background: "var(--bg-surface)",
              borderRadius: 8,
              padding: 16,
              minWidth: 0,
              overflowX: "hidden",
            }}
          >
            <Space style={{ marginBottom: 12 }}>
              <DatabaseOutlined />
              <Title level={5} style={{ margin: 0 }}>
                {t("dataAttribution.evidence")}
              </Title>
            </Space>
            <Collapse
              size="small"
              items={observations.map((obs) => ({
                key: obs.stepId,
                label: (
                  <Space wrap>
                    <Text strong>{obs.title}</Text>
                    {obs.error ? (
                      <Tag color="red">{t("common.failed")}</Tag>
                    ) : (
                      <Tag color="green">{t("common.success")}</Tag>
                    )}
                    <Tag>{obs.rowCount} rows</Tag>
                    <Tag>{formatMs(obs.elapsedMs)}</Tag>
                  </Space>
                ),
                children: (
                  <Space
                    direction="vertical"
                    size={10}
                    style={{ width: "100%" }}
                  >
                    <Text type="secondary">{obs.purpose}</Text>
                    <Paragraph
                      style={{ marginBottom: 0, overflowWrap: "anywhere" }}
                    >
                      {obs.question}
                    </Paragraph>
                    {obs.error ? (
                      <Alert type="error" showIcon message={obs.error} />
                    ) : (
                      <EvidenceTable observation={obs} />
                    )}
                    {obs.sampled ? (
                      <Alert
                        type="info"
                        showIcon
                        message={t("dataAttribution.sampledEvidence")}
                      />
                    ) : null}
                    {!!obs.usedReferences?.length && (
                      <div>
                        <Text strong>
                          <FileSearchOutlined />{" "}
                          {t("dataAttribution.usedKnowledge")}
                        </Text>
                        <div
                          style={{
                            marginTop: 6,
                            display: "flex",
                            gap: 6,
                            flexWrap: "wrap",
                          }}
                        >
                          {obs.usedReferences.slice(0, 8).map((ref) => (
                            <Tag key={`${ref.fileId}-${ref.chunkId}`}>
                              {ref.filename}:{ref.startLine}
                            </Tag>
                          ))}
                        </div>
                      </div>
                    )}
                    {!!obs.sqls?.length && (
                      <Collapse
                        size="small"
                        ghost
                        items={[
                          {
                            key: "sql",
                            label: t("dataAttribution.sqlDetail"),
                            children: (
                              <pre
                                style={{
                                  whiteSpace: "pre-wrap",
                                  overflowWrap: "anywhere",
                                  maxWidth: "100%",
                                  overflowX: "auto",
                                  margin: 0,
                                  fontSize: 12,
                                }}
                              >
                                {obs.sqls.join("\n\n")}
                              </pre>
                            ),
                          },
                        ]}
                      />
                    )}
                  </Space>
                ),
              }))}
            />
          </section>
        )}
      </div>
      <Drawer
        title={
          <Space>
            <HistoryOutlined />
            <span>{t("dataAttribution.history")}</span>
          </Space>
        }
        open={historyOpen}
        onClose={() => setHistoryOpen(false)}
        width={460}
      >
        {historyLoading ? (
          <div style={{ padding: 24, textAlign: "center" }}>
            <Spin />
          </div>
        ) : !attributionConversations?.conversations?.length ? (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={t("dataAttribution.noHistory")}
          />
        ) : (
          <div style={{ display: "grid", gap: 10 }}>
            {attributionConversations.conversations.map(
              (conv: AttributionConversationItem) => (
                <Card
                  key={conv.id}
                  size="small"
                  hoverable
                  onClick={() => handleLoadConversation(conv.id)}
                  style={{ cursor: "pointer" }}
                  title={
                    <Space size={6} style={{ maxWidth: 300 }}>
                      <MessageOutlined style={{ color: "#1677ff" }} />
                      <Text ellipsis style={{ maxWidth: 260 }}>
                        {conv.lastQuestion ||
                          t("dataAttribution.untitledConversation")}
                      </Text>
                    </Space>
                  }
                  extra={
                    <Space size={6}>
                      {loadingConversationId === conv.id ? (
                        <Spin size="small" />
                      ) : null}
                      <Popconfirm
                        title={t("dataAttribution.deleteHistoryConfirm")}
                        okText={t("common.delete")}
                        cancelText={t("common.cancel")}
                        onConfirm={(event) => {
                          event?.stopPropagation();
                          handleDeleteConversation(conv.id);
                        }}
                      >
                        <Button
                          type="text"
                          size="small"
                          icon={<DeleteOutlined />}
                          onClick={(event) => event.stopPropagation()}
                        />
                      </Popconfirm>
                    </Space>
                  }
                >
                  {conv.summary ? (
                    <Paragraph
                      type="secondary"
                      style={{
                        marginBottom: 8,
                        display: "-webkit-box",
                        WebkitLineClamp: 3,
                        WebkitBoxOrient: "vertical",
                        overflow: "hidden",
                      }}
                    >
                      {conv.summary}
                    </Paragraph>
                  ) : null}
                  <Space size={8} wrap>
                    <Tag>
                      {t("dataAttribution.historyTurns", {
                        count: conv.messageCount,
                      })}
                    </Tag>
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      {conv.updatedAt}
                    </Text>
                  </Space>
                </Card>
              ),
            )}
          </div>
        )}
      </Drawer>
    </div>
  );
}
