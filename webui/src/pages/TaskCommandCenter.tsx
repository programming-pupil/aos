import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Button,
  Descriptions,
  Empty,
  Form,
  Input,
  InputNumber,
  List,
  Modal,
  Popconfirm,
  Progress,
  Select,
  Space,
  Spin,
  Switch,
  Tabs,
  Tag,
  Timeline,
  Tooltip,
  Typography,
  message,
} from "antd";
import {
  BellOutlined,
  CheckOutlined,
  CloseOutlined,
  DeleteOutlined,
  FileSearchOutlined,
  LinkOutlined,
  ReloadOutlined,
  RetweetOutlined,
  SendOutlined,
  SettingOutlined,
  StopOutlined,
} from "@ant-design/icons";
import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { useNavigate, useSearchParams } from "@/router";
import { queryKeys } from "@/api/queryKeys";
import { taskEventMessage } from "@/api/taskEventReducer";
import {
  parseTaskTimestamp,
  tasksApi,
  type ExternalIdentity,
  type TaskBucket,
  type TaskArtifactContent,
  type TaskItem,
  type WatchRule,
  type WatchRulePendingAction,
} from "@/api/tasks";
import "./TaskCommandCenter.css";

const { Paragraph, Text, Title } = Typography;

type ViewKey = TaskBucket | "settings";

const WAITING = new Set(["waiting_input", "waiting_approval", "blocked"]);

function statusColor(status: string): string {
  if (WAITING.has(status)) return "gold";
  if (status === "completed") return "success";
  if (["failed", "timed_out", "stale"].includes(status)) return "error";
  if (status === "cancelled") return "default";
  return "processing";
}

function formatTime(value?: string | null): string {
  if (!value) return "-";
  const date = new Date(parseTaskTimestamp(value));
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

function TaskRow({
  task,
  selected,
  onClick,
}: {
  task: TaskItem;
  selected: boolean;
  onClick: () => void;
}) {
  const { t } = useTranslation();
  const activity = task.progress?.activityText ?? task.lastEvent ?? task.phase;
  const measurable = task.progress
    ? task.progress.progressKind === "percent"
    : task.progressPercent > 0;
  return (
    <div className="task-list-row" data-selected={selected} onClick={onClick}>
      <div className="task-list-row-content">
        <div className="task-list-row-header">
          <Text
            strong
            ellipsis={{ tooltip: task.title }}
            className="task-list-row-title"
          >
            {task.title}
          </Text>
          <Text code style={{ fontSize: 11 }}>
            #{task.shortCode}
          </Text>
        </div>
        <div className="task-list-row-tags">
          <Tag color={statusColor(task.status)}>
            {t(`tasks.status.${task.status}`, task.status)}
          </Tag>
          <Tag>{task.sourceLabel ?? task.capabilityKey}</Tag>
          {task.externalPlatform ? (
            <Tag icon={<SendOutlined />}>{task.externalPlatform}</Tag>
          ) : null}
        </div>
        <Text className="task-list-row-activity" type="secondary" ellipsis={{ tooltip: activity }}>
          {activity}
        </Text>
        {measurable ? (
          <Progress
            percent={task.progressPercent}
            size="small"
            showInfo={false}
          />
        ) : null}
        <Text type="secondary" style={{ fontSize: 12 }}>
          {formatTime(task.updatedAt)}
        </Text>
      </div>
    </div>
  );
}

function TaskSettings() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [pairPlatform, setPairPlatform] = useState<string>();
  const [pairCode, setPairCode] = useState<{
    code: string;
    expiresInSeconds: number;
  } | null>(null);
  const [mobileFollowEnabled, setMobileFollowEnabled] = useState(
    () => localStorage.getItem("aos-mobile-follow") === "true",
  );
  const [deliveryPage, setDeliveryPage] = useState(1);
  const deliveryPageSize = 10;
  const [ruleForm] = Form.useForm();
  const ruleActionType = Form.useWatch("actionType", ruleForm);
  const ruleDestinationType = Form.useWatch("destinationType", ruleForm);
  const presenceQuery = useQuery({
    queryKey: [...queryKeys.tasks.all, "presence"],
    queryFn: tasksApi.presenceSettings,
  });
  const identitiesQuery = useQuery({
    queryKey: queryKeys.tasks.identities(),
    queryFn: tasksApi.identities,
  });
  const rulesQuery = useQuery({
    queryKey: queryKeys.tasks.watchRules(),
    queryFn: tasksApi.watchRules,
  });
  const pendingActionsQuery = useQuery({
    queryKey: [...queryKeys.tasks.watchRules(), "pending"],
    queryFn: tasksApi.pendingWatchRuleActions,
  });
  const deliveriesQuery = useQuery({
    queryKey: queryKeys.tasks.deliveries({
      page: deliveryPage,
      perPage: deliveryPageSize,
    }),
    queryFn: () =>
      tasksApi.deliveries({ page: deliveryPage, perPage: deliveryPageSize }),
  });
  const botDestinationOptions = useMemo(() => {
    const seen = new Set<string>();
    return (identitiesQuery.data?.items ?? []).flatMap((identity) => {
      const channelId = identity.channelId?.trim();
      const conversationId = identity.externalConversationId?.trim();
      if (
        identity.status !== "active" ||
        !channelId ||
        !conversationId ||
        seen.has(channelId)
      ) {
        return [];
      }
      seen.add(channelId);
      return [{
        value: channelId,
        label: `${identity.platform} · ${identity.displayName ?? identity.externalUserId}`,
      }];
    });
  }, [identitiesQuery.data?.items]);
  const pairingMutation = useMutation({
    mutationFn: () => tasksApi.createPairingCode(pairPlatform),
    onSuccess: setPairCode,
    onError: (error: Error) => message.error(error.message),
  });
  const revokeMutation = useMutation({
    mutationFn: (identity: ExternalIdentity) =>
      tasksApi.revokeIdentity(identity.id),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: queryKeys.tasks.identities() }),
  });
  const createRuleMutation = useMutation({
    mutationFn: (values: {
      name: string;
      eventType: string;
      noProgressMinutes?: number;
      actionType: string;
      destinationType: "webui" | "bot";
      destinationRef?: string;
      onlyWhenAway?: boolean;
    }) =>
      tasksApi.createWatchRule({
        name: values.name,
        condition: {
          eventTypes: [values.eventType],
          ...(values.noProgressMinutes
            ? { noProgressSeconds: values.noProgressMinutes * 60 }
            : {}),
        },
        action: {
          type: values.actionType,
          destinationType:
            values.actionType === "notify" ? values.destinationType : "webui",
          ...(values.actionType === "notify" && values.destinationType === "bot"
            ? { destinationRef: values.destinationRef }
            : {}),
        },
        requiresConfirmation: values.actionType !== "notify",
        enabled: true,
      }),
    onSuccess: () => {
      ruleForm.resetFields();
      queryClient.invalidateQueries({ queryKey: queryKeys.tasks.watchRules() });
    },
    onError: (error: Error) => message.error(error.message),
  });
  const deleteRuleMutation = useMutation({
    mutationFn: (rule: WatchRule) => tasksApi.deleteWatchRule(rule.id),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: queryKeys.tasks.watchRules() }),
  });
  const decisionMutation = useMutation({
    mutationFn: ({
      action,
      approve,
    }: {
      action: WatchRulePendingAction;
      approve: boolean;
    }) => tasksApi.decideWatchRuleAction(action.runId, approve),
    onSuccess: () => {
      message.success(t("tasks.settings.decisionRecorded"));
      queryClient.invalidateQueries({ queryKey: queryKeys.tasks.all });
    },
    onError: (error: Error) => message.error(error.message),
  });
  const replayDeliveryMutation = useMutation({
    mutationFn: (deliveryId: string) => tasksApi.replayDelivery(deliveryId),
    onSuccess: () => {
      message.success(t("tasks.settings.deliveryReplayQueued"));
      queryClient.invalidateQueries({
        queryKey: queryKeys.tasks.all,
      });
    },
    onError: (error: Error) => message.error(error.message),
  });

  useEffect(() => {
    if (presenceQuery.data) {
      setMobileFollowEnabled(presenceQuery.data.mobileFollowEnabled);
      localStorage.setItem(
        "aos-mobile-follow",
        String(presenceQuery.data.mobileFollowEnabled),
      );
    }
  }, [presenceQuery.data]);

  return (
    <Space direction="vertical" size={24} style={{ width: "100%" }}>
      <section>
        <Title level={5}>{t("tasks.settings.mobileHandoff")}</Title>
        <Space style={{ marginBottom: 14 }}>
          <Switch
            checked={mobileFollowEnabled}
            onChange={(enabled) => {
              setMobileFollowEnabled(enabled);
              localStorage.setItem("aos-mobile-follow", String(enabled));
              const clientIdKey = "aos-task-presence-client-id";
              const clientId =
                localStorage.getItem(clientIdKey) ?? crypto.randomUUID();
              localStorage.setItem(clientIdKey, clientId);
              void tasksApi
                .presence({
                  clientId,
                  currentPath: window.location.pathname,
                  mobileFollowEnabled: enabled,
                  ttlSeconds: 60,
                })
                .catch((error: Error) => message.error(error.message));
            }}
          />
          <Text>{t("tasks.settings.mobileFollowEnabled")}</Text>
        </Space>
        <br />
        <Space wrap>
          <Select
            allowClear
            value={pairPlatform}
            onChange={setPairPlatform}
            placeholder={t("tasks.settings.anyPlatform")}
            style={{ width: 180 }}
            options={[
              "dingtalk",
              "feishu",
              "wecom",
              "whatsapp",
              "slack",
              "discord",
              "telegram",
            ].map((value) => ({ value, label: value }))}
          />
          <Button
            icon={<LinkOutlined />}
            loading={pairingMutation.isPending}
            onClick={() => pairingMutation.mutate()}
          >
            {t("tasks.settings.createPairingCode")}
          </Button>
        </Space>
        {pairCode ? (
          <Alert
            style={{ marginTop: 12 }}
            type="info"
            showIcon
            message={
              <Text code copyable>
                {pairCode.code}
              </Text>
            }
            description={t("tasks.settings.pairingHint", {
              seconds: pairCode.expiresInSeconds,
            })}
          />
        ) : null}
        <List
          style={{ marginTop: 12 }}
          loading={identitiesQuery.isLoading}
          dataSource={identitiesQuery.data?.items ?? []}
          locale={{ emptyText: t("tasks.settings.noIdentities") }}
          renderItem={(identity) => (
            <List.Item
              actions={[
                <Popconfirm
                  key="revoke"
                  title={t("tasks.settings.revokeConfirm")}
                  onConfirm={() => revokeMutation.mutate(identity)}
                >
                  <Button type="text" danger icon={<DeleteOutlined />} />
                </Popconfirm>,
              ]}
            >
              <List.Item.Meta
                title={
                  <Space>
                    <Tag>{identity.platform}</Tag>
                    {identity.displayName ?? identity.externalUserId}
                  </Space>
                }
                description={`${formatTime(identity.verifiedAt)} · ${formatTime(identity.lastSeenAt)}`}
              />
            </List.Item>
          )}
        />
      </section>

      <section className="task-detail-section">
        <Title level={5}>{t("tasks.settings.decisionInbox")}</Title>
        <List
          loading={pendingActionsQuery.isLoading}
          dataSource={pendingActionsQuery.data?.items ?? []}
          locale={{ emptyText: t("tasks.settings.noPendingDecisions") }}
          renderItem={(action) => (
            <List.Item
              actions={[
                <Popconfirm
                  key="reject"
                  title={t("tasks.rejectConfirm")}
                  onConfirm={() =>
                    decisionMutation.mutate({ action, approve: false })
                  }
                >
                  <Button
                    danger
                    icon={<CloseOutlined />}
                    loading={
                      decisionMutation.isPending &&
                      decisionMutation.variables?.action.runId === action.runId
                    }
                  >
                    {t("tasks.actions.reject")}
                  </Button>
                </Popconfirm>,
                <Popconfirm
                  key="approve"
                  title={t("tasks.settings.confirmRuleAction")}
                  onConfirm={() =>
                    decisionMutation.mutate({ action, approve: true })
                  }
                >
                  <Button
                    type="primary"
                    icon={<CheckOutlined />}
                    loading={
                      decisionMutation.isPending &&
                      decisionMutation.variables?.action.runId === action.runId
                    }
                  >
                    {t("tasks.actions.approve")}
                  </Button>
                </Popconfirm>,
              ]}
            >
              <List.Item.Meta
                title={
                  <Space wrap>
                    <Text strong>{action.taskTitle}</Text>
                    <Text code>#{action.shortCode ?? action.taskId}</Text>
                  </Space>
                }
                description={
                  <Space direction="vertical" size={2}>
                    <Text>{action.ruleName}</Text>
                    <Space wrap>
                      <Tag>{String(action.action.type ?? "-")}</Tag>
                      <Tag color={statusColor(action.taskStatus)}>
                        {action.taskStatus}
                      </Tag>
                      <Text type="secondary">
                        {formatTime(action.createdAt)}
                      </Text>
                    </Space>
                  </Space>
                }
              />
            </List.Item>
          )}
        />
      </section>

      <section className="task-detail-section">
        <Title level={5}>{t("tasks.settings.watchRules")}</Title>
        <Form
          form={ruleForm}
          layout="inline"
          initialValues={{
            eventType: "task.stalled",
            noProgressMinutes: 10,
            actionType: "notify",
            destinationType: "webui",
          }}
          onFinish={(values) => createRuleMutation.mutate(values)}
          style={{ rowGap: 8 }}
        >
          <Form.Item name="name" rules={[{ required: true }]}>
            <Input placeholder={t("tasks.settings.ruleName")} />
          </Form.Item>
          <Form.Item name="eventType">
            <Select
              style={{ width: 170 }}
              options={[
                ["task.stalled", t("tasks.settings.events.stalled")],
                ["task.failed", t("tasks.settings.events.failed")],
                ["task.completed", t("tasks.settings.events.completed")],
                [
                  "task.waiting_input",
                  t("tasks.settings.events.waitingInput"),
                ],
                [
                  "task.waiting_approval",
                  t("tasks.settings.events.waitingApproval"),
                ],
                ["task.cancelled", t("tasks.settings.events.cancelled")],
              ].map(([value, label]) => ({ value, label }))}
            />
          </Form.Item>
          <Form.Item name="noProgressMinutes">
            <InputNumber min={1} max={1440} addonAfter={t("tasks.minutes")} />
          </Form.Item>
          <Form.Item name="actionType">
            <Select
              style={{ width: 150 }}
              options={[
                { value: "notify", label: t("tasks.settings.actions.notify") },
                {
                  value: "retry_once",
                  label: t("tasks.settings.actions.retryOnce"),
                },
                {
                  value: "request_approval",
                  label: t("tasks.settings.actions.requestApproval"),
                },
              ]}
            />
          </Form.Item>
          <Form.Item name="destinationType">
            <Select
              disabled={ruleActionType !== "notify"}
              style={{ width: 150 }}
              options={[
                { value: "webui", label: t("tasks.settings.destinations.webui") },
                { value: "bot", label: t("tasks.settings.destinations.bot") },
              ]}
            />
          </Form.Item>
          {ruleActionType === "notify" && ruleDestinationType === "bot" ? (
            <Form.Item
              name="destinationRef"
              rules={[{ required: true, message: t("tasks.settings.botDestinationRequired") }]}
            >
              <Select
                style={{ width: 220 }}
                placeholder={t("tasks.settings.botDestination")}
                options={botDestinationOptions}
                notFoundContent={t("tasks.settings.noBotDestinations")}
              />
            </Form.Item>
          ) : null}
          <Form.Item>
            <Button
              htmlType="submit"
              type="primary"
              loading={createRuleMutation.isPending}
            >
              {t("common.create")}
            </Button>
          </Form.Item>
        </Form>
        <List
          style={{ marginTop: 12 }}
          loading={rulesQuery.isLoading}
          dataSource={rulesQuery.data?.items ?? []}
          locale={{ emptyText: t("tasks.settings.noRules") }}
          renderItem={(rule) => (
            <List.Item
              actions={[
                <Popconfirm
                  key="delete"
                  title={t("common.deleteConfirm")}
                  onConfirm={() => deleteRuleMutation.mutate(rule)}
                >
                  <Button type="text" danger icon={<DeleteOutlined />} />
                </Popconfirm>,
              ]}
            >
              <List.Item.Meta
                title={
                  <Space>
                    {rule.name}
                    <Tag color={rule.enabled ? "success" : "default"}>
                      {rule.enabled
                        ? t("common.enabled")
                        : t("common.disabled")}
                    </Tag>
                  </Space>
                }
                description={
                  <Text code>
                    {JSON.stringify({
                      condition: rule.condition,
                      action: rule.action,
                    })}
                  </Text>
                }
              />
            </List.Item>
          )}
        />
      </section>

      <section className="task-detail-section">
        <Title level={5}>{t("tasks.settings.deliveryHealth")}</Title>
        <Text type="secondary" style={{ display: "block", marginBottom: 12 }}>
          {t("tasks.settings.deliveryHealthDescription")}
        </Text>
        <List
          loading={deliveriesQuery.isLoading}
          dataSource={deliveriesQuery.data?.items ?? []}
          pagination={{
            current: deliveryPage,
            pageSize: deliveryPageSize,
            total: deliveriesQuery.data?.total ?? 0,
            hideOnSinglePage: true,
            showSizeChanger: false,
            onChange: setDeliveryPage,
          }}
          locale={{ emptyText: t("tasks.settings.noDeliveries") }}
          renderItem={(delivery) => (
            <List.Item
              actions={
                delivery.allowedActions?.includes("replay")
                  ? [
                      <Popconfirm
                        key="replay"
                        title={t("tasks.settings.replayDeliveryConfirm")}
                        onConfirm={() =>
                          replayDeliveryMutation.mutate(delivery.id)
                        }
                      >
                        <Button
                          type="text"
                          icon={<RetweetOutlined />}
                          loading={
                            replayDeliveryMutation.isPending &&
                            replayDeliveryMutation.variables === delivery.id
                          }
                        >
                          {t("tasks.settings.replayDelivery")}
                        </Button>
                      </Popconfirm>,
                    ]
                  : undefined
              }
            >
              <List.Item.Meta
                title={
                  <Space>
                    <Tag>{delivery.platform}</Tag>
                    <Tag
                      color={
                        delivery.status === "sent"
                          ? "success"
                          : delivery.status === "failed"
                            ? "error"
                            : delivery.status === "unknown"
                              ? "warning"
                              : "processing"
                      }
                    >
                      {delivery.status}
                    </Tag>
                    {delivery.title}
                  </Space>
                }
                description={
                  delivery.status === "unknown"
                    ? t("tasks.settings.deliveryReceiptUnknown")
                    : (delivery.lastError ??
                      `${delivery.attemptCount}/${delivery.maxAttempts} · ${formatTime(delivery.updatedAt)}`)
                }
              />
            </List.Item>
          )}
        />
      </section>
    </Space>
  );
}

function TaskDetail({ taskId }: { taskId?: string }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [inputOpen, setInputOpen] = useState(false);
  const [inputText, setInputText] = useState("");
  const [pendingCancelTaskId, setPendingCancelTaskId] = useState<string>();
  const [artifactContent, setArtifactContent] =
    useState<TaskArtifactContent | null>(null);
  const [artifactLoadingId, setArtifactLoadingId] = useState<string>();
  const detailQuery = useQuery({
    queryKey: queryKeys.tasks.detail(taskId ?? ""),
    queryFn: () => tasksApi.detail(taskId ?? ""),
    enabled: !!taskId,
  });
  const eventsQuery = useQuery({
    queryKey: queryKeys.tasks.events(taskId ?? ""),
    queryFn: () => tasksApi.events(taskId ?? "", { limit: 200 }),
    enabled: !!taskId,
  });
  const resourcesQuery = useQuery({
    queryKey: queryKeys.tasks.resources(taskId ?? ""),
    queryFn: () => tasksApi.resources(taskId ?? ""),
    enabled: !!taskId,
  });
  const artifactsQuery = useQuery({
    queryKey: queryKeys.tasks.artifacts(taskId ?? ""),
    queryFn: () => tasksApi.artifacts(taskId ?? ""),
    enabled: !!taskId,
  });
  const attemptsQuery = useQuery({
    queryKey: queryKeys.tasks.attempts(taskId ?? ""),
    queryFn: () => tasksApi.attempts(taskId ?? ""),
    enabled: !!taskId,
  });
  const commandsQuery = useQuery({
    queryKey: queryKeys.tasks.commands(taskId ?? ""),
    queryFn: () => tasksApi.commands(taskId ?? ""),
    enabled: !!taskId,
  });
  const subscriptionsQuery = useQuery({
    queryKey: queryKeys.tasks.subscriptions(taskId ?? ""),
    queryFn: () => tasksApi.subscriptions(taskId ?? ""),
    enabled: !!taskId,
  });
  const commandMutation = useMutation({
    mutationFn: ({
      commandType,
      input,
    }: {
      commandType: string;
      input?: unknown;
    }) => {
      const task = detailQuery.data;
      if (!task) throw new Error("Task is unavailable");
      return tasksApi.command(task.id, commandType, {
        input,
        expectedStateVersion: task.stateVersion,
        idempotencyKey: `webui:${commandType}:${task.id}:${task.stateVersion}`,
      });
    },
    onSuccess: () => {
      setInputOpen(false);
      setInputText("");
      queryClient.invalidateQueries({ queryKey: queryKeys.tasks.all });
    },
    onError: (error: Error, variables) => {
      if (variables.commandType === "cancel") {
        setPendingCancelTaskId(undefined);
      }
      message.error(error.message);
    },
  });
  const subscribeMutation = useMutation({
    mutationFn: () =>
      tasksApi.subscribe(taskId ?? "", {
        eventTypes: [
          "task.completed",
          "task.failed",
          "task.waiting_input",
          "task.waiting_approval",
          "task.stalled",
        ],
        destinationType: "webui",
      }),
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: queryKeys.tasks.subscriptions(taskId ?? ""),
      }),
  });
  const unsubscribeMutation = useMutation({
    mutationFn: (id: string) => tasksApi.unsubscribe(taskId ?? "", id),
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: queryKeys.tasks.subscriptions(taskId ?? ""),
      }),
  });
  useEffect(() => {
    if (!pendingCancelTaskId) return;
    const timer = window.setTimeout(
      () => setPendingCancelTaskId(undefined),
      30_000,
    );
    return () => window.clearTimeout(timer);
  }, [pendingCancelTaskId]);
  useEffect(() => {
    const task = detailQuery.data;
    if (!task || task.id !== pendingCancelTaskId) return;
    if (
      !task.allowedActions?.includes("cancel") ||
      ["cancelling", "cancelled", "completed", "failed", "timed_out", "stale"].includes(
        task.status,
      )
    ) {
      setPendingCancelTaskId(undefined);
    }
  }, [detailQuery.data, pendingCancelTaskId]);
  const openArtifact = async (artifactId: string) => {
    setArtifactLoadingId(artifactId);
    try {
      setArtifactContent(
        await tasksApi.artifactContent(taskId ?? "", artifactId),
      );
    } catch (error) {
      message.error((error as Error).message);
    } finally {
      setArtifactLoadingId(undefined);
    }
  };

  if (!taskId) return <Empty description={t("tasks.selectTask")} />;
  if (detailQuery.isLoading) return <Spin />;
  if (detailQuery.error || !detailQuery.data)
    return (
      <Alert
        type="error"
        showIcon
        message={
          (detailQuery.error as Error)?.message ?? t("tasks.taskUnavailable")
        }
      />
    );
  const task = detailQuery.data;
  const cancelPending = pendingCancelTaskId === task.id;
  const cancelTask = () => {
    if (cancelPending || commandMutation.isPending) return;
    setPendingCancelTaskId(task.id);
    commandMutation.mutate({ commandType: "cancel" });
  };
  const subscription = subscriptionsQuery.data?.items.find(
    (item) => item.destinationType === "webui",
  );
  const openOriginal = () => {
    if (task.originSessionId) {
      navigate(
        `/super-assistant?sessionId=${encodeURIComponent(task.originSessionId)}${task.originTurnId ? `&turnId=${encodeURIComponent(task.originTurnId)}` : ""}`,
      );
    }
  };

  return (
    <Space direction="vertical" size={0} style={{ width: "100%" }}>
      <section className="task-detail-section">
        <Space
          style={{ width: "100%", justifyContent: "space-between" }}
          align="start"
          wrap
        >
          <div style={{ minWidth: 0 }}>
            <Title level={4} style={{ margin: 0 }}>
              {task.title}
            </Title>
            <Space wrap style={{ marginTop: 8 }}>
              <Text code>#{task.shortCode}</Text>
              <Tag color={statusColor(task.status)}>
                {t(`tasks.status.${task.status}`, task.status)}
              </Tag>
              <Tag>{task.capabilityKey}</Tag>
              <Tag>{task.sensitivityLabel}</Tag>
            </Space>
          </div>
          <Space wrap>
            {task.originSessionId ? (
              <Button icon={<LinkOutlined />} onClick={openOriginal}>
                {t("tasks.openOriginal")}
              </Button>
            ) : null}
            {subscription ? (
              <Button
                icon={<BellOutlined />}
                onClick={() => unsubscribeMutation.mutate(subscription.id)}
              >
                {t("tasks.unfollow")}
              </Button>
            ) : (
              <Button
                icon={<BellOutlined />}
                loading={subscribeMutation.isPending}
                onClick={() => subscribeMutation.mutate()}
              >
                {t("tasks.follow")}
              </Button>
            )}
          </Space>
        </Space>
        <Paragraph type="secondary" style={{ marginTop: 12 }}>
          {task.summary ?? task.lastEvent}
        </Paragraph>
        {(
          task.progress
            ? task.progress.progressKind === "percent"
            : task.progressPercent > 0
        ) ? (
          <Progress percent={task.progressPercent} />
        ) : null}
        <Descriptions
          size="small"
          column={{ xs: 1, sm: 2, lg: 3 }}
          style={{ marginTop: 12 }}
        >
          <Descriptions.Item label={t("tasks.phase")}>
            {task.progress?.phaseLabel ?? task.phase}
          </Descriptions.Item>
          <Descriptions.Item label={t("tasks.lastActivity")}>
            {task.progress?.activityText ?? task.lastEvent ?? "-"}
          </Descriptions.Item>
          <Descriptions.Item label={t("common.updatedAt")}>
            {formatTime(task.updatedAt)}
          </Descriptions.Item>
          <Descriptions.Item label={t("tasks.source")}>
            {task.sourceLabel ?? task.source}
          </Descriptions.Item>
          <Descriptions.Item label={t("tasks.startedAt")}>
            {formatTime(task.startedAt)}
          </Descriptions.Item>
          <Descriptions.Item label={t("tasks.sla")}>
            {formatTime(task.slaDueAt)}
          </Descriptions.Item>
        </Descriptions>
        {task.errorMessage ? (
          <Alert
            style={{ marginTop: 12 }}
            type="error"
            showIcon
            message={task.errorCode ?? t("common.error")}
            description={task.errorMessage}
          />
        ) : null}
        {task.resultSummary ? (
          <Alert
            style={{ marginTop: 12 }}
            type="success"
            showIcon
            message={t("tasks.result")}
            description={task.resultSummary}
          />
        ) : null}
        <Space wrap style={{ marginTop: 14 }}>
          {task.allowedActions?.includes("cancel") ? (
            <Popconfirm
              title={t("tasks.cancelConfirm")}
              onConfirm={cancelTask}
            >
              <Button
                danger
                icon={<StopOutlined />}
                loading={cancelPending}
                disabled={commandMutation.isPending && !cancelPending}
              >
                {t("tasks.actions.cancel")}
              </Button>
            </Popconfirm>
          ) : null}
          {task.allowedActions?.includes("retry") ? (
            <Popconfirm
              title={t("tasks.retryConfirm")}
              onConfirm={() => commandMutation.mutate({ commandType: "retry" })}
            >
              <Button icon={<RetweetOutlined />}>
                {t("tasks.actions.retry")}
              </Button>
            </Popconfirm>
          ) : null}
          {task.allowedActions?.includes("provide_input") ? (
            <Button type="primary" onClick={() => setInputOpen(true)}>
              {t("tasks.actions.provideInput")}
            </Button>
          ) : null}
          {task.allowedActions?.includes("approve") ? (
            <Popconfirm
              title={t("tasks.approveConfirm")}
              onConfirm={() =>
                commandMutation.mutate({ commandType: "approve" })
              }
            >
              <Button type="primary" icon={<CheckOutlined />}>
                {t("tasks.actions.approve")}
              </Button>
            </Popconfirm>
          ) : null}
          {task.allowedActions?.includes("reject") ? (
            <Popconfirm
              title={t("tasks.rejectConfirm")}
              onConfirm={() =>
                commandMutation.mutate({ commandType: "reject" })
              }
            >
              <Button danger icon={<CloseOutlined />}>
                {t("tasks.actions.reject")}
              </Button>
            </Popconfirm>
          ) : null}
        </Space>
      </section>

      <section className="task-detail-section">
        <Title level={5}>{t("tasks.timeline")}</Title>
        <Timeline
          items={(eventsQuery.data?.items ?? []).map((event) => ({
            color:
              event.payload?.severity === "error"
                ? "red"
                : event.payload?.severity === "warn"
                  ? "orange"
                  : "blue",
            children: (
              <div>
                <Space wrap>
                  <Text strong>{taskEventMessage(event)}</Text>
                  <Text type="secondary">{formatTime(event.createdAt)}</Text>
                </Space>
                <div>
                  <Text code>{event.eventType}</Text>
                </div>
              </div>
            ),
          }))}
        />
        {!eventsQuery.isLoading && !eventsQuery.data?.items.length ? (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} />
        ) : null}
      </section>

      <section className="task-detail-section">
        <Title level={5}>{t("tasks.executionGraph")}</Title>
        <List
          size="small"
          dataSource={resourcesQuery.data?.items ?? []}
          locale={{ emptyText: t("tasks.noResources") }}
          renderItem={(resource) => (
            <List.Item>
              <Space wrap>
                <Tag>{resource.relationType}</Tag>
                <Text>{resource.resourceType}</Text>
                <Text code copyable>
                  {resource.resourceId}
                </Text>
              </Space>
            </List.Item>
          )}
        />
        <List
          size="small"
          header={<Text strong>{t("tasks.attempts")}</Text>}
          dataSource={attemptsQuery.data?.items ?? []}
          locale={{ emptyText: t("tasks.noAttempts") }}
          renderItem={(attempt) => (
            <List.Item>
              <Space wrap>
                <Tag color={statusColor(attempt.status)}>{attempt.status}</Tag>
                <Text>{attempt.triggerType}</Text>
                <Text>#{attempt.attemptNo}</Text>
                <Text type="secondary">
                  {formatTime(attempt.startedAt ?? attempt.createdAt)}
                </Text>
              </Space>
            </List.Item>
          )}
        />
      </section>

      <section className="task-detail-section">
        <Title level={5}>{t("tasks.artifacts")}</Title>
        <List
          size="small"
          dataSource={artifactsQuery.data?.items ?? []}
          locale={{ emptyText: t("tasks.noArtifacts") }}
          renderItem={(artifact) => (
            <List.Item
              actions={[
                <Tooltip key="open" title={t("tasks.openArtifact")}>
                  <Button
                    type="text"
                    icon={<FileSearchOutlined />}
                    loading={artifactLoadingId === artifact.id}
                    onClick={() => void openArtifact(artifact.id)}
                  />
                </Tooltip>,
              ]}
            >
              <Space direction="vertical" size={2}>
                <Space wrap>
                  <Tag>{artifact.artifactType}</Tag>
                  <Text strong>{artifact.name}</Text>
                  <Text type="secondary">
                    {formatBytes(artifact.sizeBytes)}
                  </Text>
                </Space>
                <Text code copyable>
                  {artifact.artifactRef}
                </Text>
              </Space>
            </List.Item>
          )}
        />
      </section>

      <section className="task-detail-section">
        <Title level={5}>{t("tasks.commandAudit")}</Title>
        <List
          size="small"
          dataSource={commandsQuery.data?.items ?? []}
          locale={{ emptyText: t("tasks.noCommands") }}
          renderItem={(command) => (
            <List.Item>
              <Space direction="vertical" size={2}>
                <Space wrap>
                  <Tag>{command.commandType}</Tag>
                  <Tag
                    color={
                      command.status === "succeeded"
                        ? "success"
                        : command.status === "failed"
                          ? "error"
                          : "processing"
                    }
                  >
                    {command.status}
                  </Tag>
                  <Text type="secondary">{formatTime(command.createdAt)}</Text>
                </Space>
                {command.errorMessage ? (
                  <Text type="danger">{command.errorMessage}</Text>
                ) : null}
              </Space>
            </List.Item>
          )}
        />
      </section>

      <Modal
        title={t("tasks.actions.provideInput")}
        open={inputOpen}
        onCancel={() => setInputOpen(false)}
        onOk={() =>
          commandMutation.mutate({
            commandType: "provide_input",
            input: { text: inputText },
          })
        }
        okButtonProps={{
          disabled: !inputText.trim(),
          loading: commandMutation.isPending,
        }}
      >
        <Input.TextArea
          rows={5}
          value={inputText}
          onChange={(event) => setInputText(event.target.value)}
        />
      </Modal>
      <Modal
        title={artifactContent?.name ?? t("tasks.artifacts")}
        open={!!artifactContent}
        footer={null}
        width={900}
        destroyOnHidden
        onCancel={() => setArtifactContent(null)}
      >
        <pre className="task-code-block">
          {typeof artifactContent?.content === "string"
            ? artifactContent.content
            : JSON.stringify(artifactContent?.content ?? null, null, 2)}
        </pre>
      </Modal>
    </Space>
  );
}

export default function TaskCommandCenter() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const queryClient = useQueryClient();
  const initialTaskId = searchParams.get("task") ?? undefined;
  const [view, setView] = useState<ViewKey>("active");
  const [selectedTaskId, setSelectedTaskId] = useState<string | undefined>(
    initialTaskId,
  );
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  const bucket = view === "settings" ? "active" : view;
  const tasksQuery = useInfiniteQuery({
    queryKey: queryKeys.tasks.list({ bucket, limit: 30 }),
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      tasksApi.list({ bucket, cursor: pageParam ?? undefined, limit: 30 }),
    getNextPageParam: (page) => page.nextCursor ?? undefined,
    enabled: view !== "settings",
  });
  const items = useMemo(() => {
    const seen = new Set<string>();
    return (tasksQuery.data?.pages ?? [])
      .flatMap((page) => page.items)
      .filter((task) => {
        if (seen.has(task.id)) return false;
        seen.add(task.id);
        return true;
      });
  }, [tasksQuery.data?.pages]);

  useEffect(() => {
    const node = sentinelRef.current;
    if (!node || !tasksQuery.hasNextPage) return;
    const observer = new IntersectionObserver((entries) => {
      if (
        entries.some((entry) => entry.isIntersecting) &&
        !tasksQuery.isFetchingNextPage
      ) {
        void tasksQuery.fetchNextPage();
      }
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [
    tasksQuery.fetchNextPage,
    tasksQuery.hasNextPage,
    tasksQuery.isFetchingNextPage,
  ]);

  const selectTask = (task: TaskItem) => {
    setSelectedTaskId(task.id);
    navigate(`/tasks?task=${encodeURIComponent(task.id)}`, { replace: true });
  };

  return (
    <div className="task-command-center">
      <div className="task-command-header">
        <div>
          <Title level={3} style={{ margin: 0 }}>
            {t("tasks.title")}
          </Title>
          <Text type="secondary">{t("tasks.subtitle")}</Text>
        </div>
        <Space wrap>
          <Button
            icon={<ReloadOutlined />}
            onClick={() =>
              queryClient.invalidateQueries({ queryKey: queryKeys.tasks.all })
            }
          >
            {t("common.refresh")}
          </Button>
          <Button
            icon={<SettingOutlined />}
            type={view === "settings" ? "primary" : "default"}
            onClick={() => setView("settings")}
          >
            {t("tasks.settings.title")}
          </Button>
        </Space>
      </div>
      <Tabs
        activeKey={view}
        onChange={(key) => setView(key as ViewKey)}
        style={{ margin: "0 20px" }}
        items={[
          ["active", t("tasks.tabs.active")],
          ["waiting", t("tasks.tabs.waiting")],
          ["following", t("tasks.tabs.following")],
          ["history", t("tasks.tabs.history")],
          ["settings", t("tasks.tabs.settings")],
        ].map(([key, label]) => ({ key, label }))}
      />
      {view === "settings" ? (
        <div className="task-detail-pane">
          <TaskSettings />
        </div>
      ) : (
        <div className="task-command-body">
          <div className="task-list-pane">
            {tasksQuery.isLoading ? (
              <div style={{ padding: 32, textAlign: "center" }}>
                <Spin />
              </div>
            ) : null}
            {tasksQuery.error ? (
              <Alert
                type="error"
                showIcon
                message={(tasksQuery.error as Error).message}
              />
            ) : null}
            {!tasksQuery.isLoading && !items.length ? (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description={t("tasks.empty")}
              />
            ) : null}
            {items.map((task) => (
              <TaskRow
                key={task.id}
                task={task}
                selected={selectedTaskId === task.id}
                onClick={() => selectTask(task)}
              />
            ))}
            <div ref={sentinelRef} style={{ height: 1 }} />
            {tasksQuery.isFetchingNextPage ? (
              <div style={{ padding: 12, textAlign: "center" }}>
                <Spin size="small" />
              </div>
            ) : null}
          </div>
          <div className="task-detail-pane">
            <TaskDetail taskId={selectedTaskId} />
          </div>
        </div>
      )}
    </div>
  );
}
