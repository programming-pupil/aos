import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Badge, Button, Drawer, Empty, List, Popconfirm, Progress, Space, Tag, Tooltip, Typography, message } from 'antd';
import {
  CheckCircleOutlined,
  ClockCircleOutlined,
  ExclamationCircleOutlined,
  RightOutlined,
  StopOutlined,
} from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { useLocation, useNavigate } from '@/router';
import { queryKeys } from '@/api/queryKeys';
import { initialTaskEventState, reduceTaskEvent } from '@/api/taskEventReducer';
import {
  parseTaskTimestamp,
  streamTaskEvents,
  tasksApi,
  type TaskEvent,
  type TaskItem,
} from '@/api/tasks';
import { useAuthStore } from '@/store/auth';

const { Text } = Typography;

const ACTIVE = new Set(['created', 'queued', 'claimed', 'running', 'retrying', 'cancelling']);
const WAITING = new Set(['waiting_input', 'waiting_approval', 'blocked']);

function statusColor(status: string): string {
  if (WAITING.has(status)) return 'gold';
  if (ACTIVE.has(status)) return 'processing';
  if (status === 'completed') return 'success';
  if (['failed', 'timed_out', 'stale'].includes(status)) return 'error';
  return 'default';
}

function elapsed(task: TaskItem): string {
  const start = parseTaskTimestamp(task.startedAt ?? task.createdAt);
  const end = task.completedAt ? parseTaskTimestamp(task.completedAt) : Date.now();
  if (!Number.isFinite(start) || !Number.isFinite(end)) return '';
  const seconds = Math.max(0, Math.floor((end - start) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

export function TaskCommandDrawer() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const location = useLocation();
  const queryClient = useQueryClient();
  const tenantId = useAuthStore((state) => state.tenantId);
  const userId = useAuthStore((state) => state.user?.id);
  const [open, setOpen] = useState(false);
  const streamKey = `aos-task-event-cursor:${tenantId ?? 'unknown'}:${userId ?? 'unknown'}`;
  const streamStateRef = useRef(initialTaskEventState());
  const cursorRef = useRef(0);
  const invalidateTimerRef = useRef<number | null>(null);
  const pendingTaskIdsRef = useRef(new Set<string>());
  const pendingTerminalTaskIdsRef = useRef(new Set<string>());

  const summaryQuery = useQuery({
    queryKey: queryKeys.tasks.summary('own'),
    queryFn: () => tasksApi.summary('own'),
    retry: 2,
  });
  const activeQuery = useQuery({
    queryKey: queryKeys.tasks.list({ bucket: 'active', limit: 20 }),
    queryFn: () => tasksApi.list({ bucket: 'active', limit: 20 }),
    enabled: open,
    retry: 2,
  });
  const waitingQuery = useQuery({
    queryKey: queryKeys.tasks.list({ bucket: 'waiting', limit: 20 }),
    queryFn: () => tasksApi.list({ bucket: 'waiting', limit: 20 }),
    enabled: open,
    retry: 2,
  });

  const invalidateTaskData = useCallback((event?: TaskEvent) => {
    if (event) {
      const taskId = event.rootTaskId || event.taskId;
      pendingTaskIdsRef.current.add(taskId);
      if (['completed', 'failed', 'cancelled', 'timed_out', 'stale'].includes(event.payload?.status ?? '')) {
        pendingTerminalTaskIdsRef.current.add(taskId);
      }
    }
    if (invalidateTimerRef.current !== null) return;
    invalidateTimerRef.current = window.setTimeout(() => {
      invalidateTimerRef.current = null;
      const taskIds = [...pendingTaskIdsRef.current];
      const terminalTaskIds = [...pendingTerminalTaskIdsRef.current];
      pendingTaskIdsRef.current.clear();
      pendingTerminalTaskIdsRef.current.clear();
      void queryClient.invalidateQueries({ queryKey: queryKeys.tasks.summary('own') });
      void queryClient.invalidateQueries({ queryKey: [...queryKeys.tasks.all, 'list'] });
      for (const taskId of taskIds) {
        void queryClient.invalidateQueries({ queryKey: queryKeys.tasks.detail(taskId) });
        void queryClient.invalidateQueries({ queryKey: queryKeys.tasks.events(taskId) });
      }
      for (const taskId of terminalTaskIds) {
        void queryClient.invalidateQueries({ queryKey: queryKeys.tasks.resources(taskId) });
        void queryClient.invalidateQueries({ queryKey: queryKeys.tasks.artifacts(taskId) });
        void queryClient.invalidateQueries({ queryKey: queryKeys.tasks.attempts(taskId) });
        void queryClient.invalidateQueries({ queryKey: queryKeys.tasks.commands(taskId) });
      }
    }, 1_000);
  }, [queryClient]);

  useEffect(() => {
    const stored = Number(localStorage.getItem(streamKey) ?? 0);
    const cursor = Number.isFinite(stored) ? Math.max(0, stored) : 0;
    streamStateRef.current = initialTaskEventState(cursor);
    cursorRef.current = cursor;
  }, [streamKey]);

  useEffect(() => {
    if (!tenantId || !userId) return;
    let stopped = false;
    let reconnectDelay = 500;
    let controller: AbortController | null = null;
    const connect = async () => {
      while (!stopped) {
        controller = new AbortController();
        try {
          await streamTaskEvents({
            afterEventId: cursorRef.current,
            signal: controller.signal,
            onEvent: (event) => {
              const previous = streamStateRef.current;
              const next = reduceTaskEvent(previous, event);
              if (next === previous) return;
              streamStateRef.current = next;
              cursorRef.current = next.lastEventId;
              localStorage.setItem(streamKey, String(next.lastEventId));
              invalidateTaskData(event);
            },
          });
          reconnectDelay = 500;
          if (!stopped) {
            await new Promise((resolve) => window.setTimeout(resolve, reconnectDelay));
          }
        } catch (error) {
          if (controller.signal.aborted || stopped) return;
          await new Promise((resolve) => window.setTimeout(resolve, reconnectDelay));
          reconnectDelay = Math.min(10_000, reconnectDelay * 2);
        }
      }
    };
    void connect();
    return () => {
      stopped = true;
      controller?.abort();
    };
  }, [invalidateTaskData, streamKey, tenantId, userId]);

  useEffect(() => {
    if (!tenantId || !userId) return;
    const clientIdKey = 'aos-task-presence-client-id';
    const clientId = localStorage.getItem(clientIdKey) ?? crypto.randomUUID();
    localStorage.setItem(clientIdKey, clientId);
    const heartbeat = () => {
      void tasksApi.presence({
        clientId,
        currentPath: location.pathname,
        ttlSeconds: 60,
      }).catch(() => undefined);
    };
    heartbeat();
    const timer = window.setInterval(heartbeat, 30_000);
    return () => window.clearInterval(timer);
  }, [location.pathname, tenantId, userId]);

  useEffect(
    () => () => {
      if (invalidateTimerRef.current !== null) window.clearTimeout(invalidateTimerRef.current);
    },
    [],
  );

  const cancelMutation = useMutation({
    mutationFn: (task: TaskItem) =>
      tasksApi.command(task.id, 'cancel', {
        expectedStateVersion: task.stateVersion,
        idempotencyKey: `webui-cancel:${task.id}:${task.stateVersion}`,
      }),
    onSuccess: () => invalidateTaskData(),
    onError: (error: Error) => message.error(error.message),
  });

  const waiting = waitingQuery.data?.items ?? [];
  const active = activeQuery.data?.items ?? [];
  const visibleTasks = useMemo(() => [...waiting, ...active], [active, waiting]);
  const badgeCount = (summaryQuery.data?.running ?? 0) + (summaryQuery.data?.waiting ?? 0);

  const openTask = (task: TaskItem) => {
    setOpen(false);
    navigate(`/tasks?task=${encodeURIComponent(task.id)}`);
  };

  return (
    <>
      <Tooltip title={t('tasks.openDrawer')}>
        <Badge count={badgeCount} size="small" overflowCount={99}>
          <Button
            type="text"
            icon={<ClockCircleOutlined />}
            aria-label={t('tasks.openDrawer')}
            onClick={() => setOpen(true)}
          />
        </Badge>
      </Tooltip>
      <Drawer
        title={t('tasks.drawerTitle')}
        open={open}
        onClose={() => setOpen(false)}
        width="min(440px, 100vw)"
        styles={{ body: { padding: 0 } }}
        extra={
          <Button type="link" onClick={() => { setOpen(false); navigate('/tasks'); }}>
            {t('tasks.openCommandCenter')}
          </Button>
        }
      >
        {visibleTasks.length === 0 ? (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('tasks.noActiveTasks')} />
        ) : (
          <List
            dataSource={visibleTasks}
            renderItem={(task) => (
              <List.Item
                style={{ padding: '14px 16px', cursor: 'pointer' }}
                onClick={() => openTask(task)}
                actions={[
                  task.allowedActions?.includes('cancel') ? (
                    <Popconfirm
                      key="cancel"
                      title={t('tasks.cancelConfirm')}
                      onConfirm={(event) => {
                        event?.stopPropagation();
                        cancelMutation.mutate(task);
                      }}
                      onCancel={(event) => event?.stopPropagation()}
                    >
                      <Tooltip title={t('tasks.actions.cancel')}>
                        <Button
                          type="text"
                          danger
                          icon={<StopOutlined />}
                          loading={cancelMutation.isPending && cancelMutation.variables?.id === task.id}
                          onClick={(event) => event.stopPropagation()}
                        />
                      </Tooltip>
                    </Popconfirm>
                  ) : (
                    <RightOutlined key="open" />
                  ),
                ]}
              >
                <List.Item.Meta
                  avatar={
                    WAITING.has(task.status) ? (
                      <ExclamationCircleOutlined style={{ color: 'var(--warning)' }} />
                    ) : task.status === 'completed' ? (
                      <CheckCircleOutlined style={{ color: 'var(--success)' }} />
                    ) : (
                      <ClockCircleOutlined style={{ color: 'var(--accent-ai)' }} />
                    )
                  }
                  title={
                    <Space size={6} wrap>
                      <Text strong ellipsis style={{ maxWidth: 230 }}>{task.title}</Text>
                      <Text code style={{ fontSize: 11 }}>#{task.shortCode}</Text>
                    </Space>
                  }
                  description={
                    <Space direction="vertical" size={5} style={{ width: '100%' }}>
                      <Space size={6} wrap>
                        <Tag color={statusColor(task.status)}>{t(`tasks.status.${task.status}`, task.status)}</Tag>
                        <Text type="secondary">{task.progress?.activityText ?? task.lastEvent ?? task.phase}</Text>
                      </Space>
                      {(task.progress ? task.progress.progressKind === 'percent' : task.progressPercent > 0) ? (
                        <Progress percent={task.progressPercent} size="small" showInfo={false} />
                      ) : null}
                      <Text type="secondary" style={{ fontSize: 12 }}>{elapsed(task)}</Text>
                    </Space>
                  }
                />
              </List.Item>
            )}
          />
        )}
      </Drawer>
    </>
  );
}
