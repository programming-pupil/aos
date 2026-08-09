import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, Button, Empty, Form, Input, InputNumber, Space, Spin, Tag, Typography, message } from 'antd';
import {
  BugOutlined,
  CameraOutlined,
  PlayCircleOutlined,
  ReloadOutlined,
  StopOutlined,
} from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { RdPreviewSession, RdRepository } from '@/types';
import { PreviewLogsPanel } from './PreviewLogsPanel';

const { Text } = Typography;

function previewStatusColor(status?: string) {
  if (status === 'running') return 'green';
  if (status === 'starting') return 'blue';
  if (status === 'failed') return 'red';
  if (status === 'stopped') return 'default';
  return 'default';
}

function defaultPreviewCommand(repository?: RdRepository | null) {
  const stack = repository?.detectedStack ?? [];
  if (stack.some((item) => /vite/i.test(item))) return 'npm run dev -- --host 127.0.0.1';
  if (stack.some((item) => /next/i.test(item))) return 'npm run dev';
  if (repository?.detectedBuildCommand?.includes('pnpm')) return 'pnpm dev';
  return 'npm run dev';
}

function previewEvidencePrompt(prefix: string, session: RdPreviewSession | null, manualIssue: string, events: Array<{ eventType: string; severity: string; message: string }> = []) {
  const lines = [prefix.trim(), ''];
  if (session) {
    lines.push(`[Preview Session]`);
    lines.push(`sessionId: ${session.id}`);
    lines.push(`status: ${session.status}`);
    lines.push(`url: ${session.url ?? ''}`);
    lines.push(`proxiedUrl: ${session.proxiedUrl ?? ''}`);
    if (session.lastError) lines.push(`lastError: ${session.lastError}`);
    if (session.logsPreview) {
      lines.push('');
      lines.push(`[Runtime Logs Preview]`);
      lines.push(session.logsPreview.slice(0, 2000));
    }
  }
  const trimmedIssue = manualIssue.trim();
  if (trimmedIssue) {
    lines.push('');
    lines.push('[User Observed Issue]');
    lines.push(trimmedIssue);
  }
  if (events.length > 0) {
    lines.push('');
    lines.push('[Recent Preview Events]');
    for (const event of events.slice(0, 8)) {
      lines.push(`- ${event.severity} ${event.eventType}: ${event.message}`);
    }
  }
  lines.push('');
  lines.push('请先根据这些预览调试证据定位 root cause，读取真实文件后再修改 candidate workspace，并在最终回答里说明验证方式。');
  return lines.join('\n');
}

export function PreviewPanel({
  repository,
  taskId,
  onFixWithAgent,
  onSessionChange,
}: {
  repository?: RdRepository | null;
  taskId?: string | null;
  onFixWithAgent?: (prompt: string) => void;
  onSessionChange?: (session: RdPreviewSession | null) => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [session, setSession] = useState<RdPreviewSession | null>(null);
  const [command, setCommand] = useState(defaultPreviewCommand(repository));
  const [port, setPort] = useState<number | null>(5173);
  const [path, setPath] = useState('/');
  const [iframeKey, setIframeKey] = useState(0);
  const [manualIssue, setManualIssue] = useState('');
  const [capturedEvents, setCapturedEvents] = useState<Array<{ eventType: string; severity: string; message: string }>>([]);
  const [authorizedPreviewUrl, setAuthorizedPreviewUrl] = useState('');
  const previewFrameRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    onSessionChange?.(session);
  }, [onSessionChange, session]);

  useEffect(() => {
    setCommand(defaultPreviewCommand(repository));
  }, [repository?.id]);

  const sessionQuery = useQuery({
    queryKey: queryKeys.rd.previewSession(session?.id),
    queryFn: () => rdApi.getPreviewSession(session!.id),
    enabled: !!session?.id,
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status && ['starting', 'running'].includes(status) ? 3000 : false;
    },
  });

  useEffect(() => {
    if (sessionQuery.data) setSession(sessionQuery.data);
  }, [sessionQuery.data]);

  const startMutation = useMutation({
    mutationFn: () => rdApi.createPreviewSession(repository!.id, {
      command,
      port: port ?? undefined,
      path,
      taskId: taskId ?? undefined,
    }),
    onSuccess: (next) => {
      setSession(next);
      message.success(t('rd.previewStarted', '预览已启动'));
      void queryClient.invalidateQueries({ queryKey: queryKeys.rd.previewLogs(next.id) });
    },
    onError: (error) => {
      message.error((error as Error).message || t('rd.previewStartFailed', '预览启动失败'));
    },
  });

  const stopMutation = useMutation({
    mutationFn: () => rdApi.stopPreviewSession(session!.id),
    onSuccess: (next) => {
      setSession(next);
      message.success(t('rd.previewStopped', '预览已停止'));
      void queryClient.invalidateQueries({ queryKey: queryKeys.rd.previewLogs(next.id) });
    },
  });

  const screenshotMutation = useMutation({
    mutationFn: () => rdApi.previewScreenshot(session!.id),
    onSuccess: () => message.success(t('rd.previewScreenshotRecorded', '已记录截图请求')),
  });

  const consoleMutation = useMutation({
    mutationFn: (payload: { message: string; eventType?: string; severity?: string; metadataJson?: Record<string, unknown>; silent?: boolean }) => rdApi.recordPreviewConsoleEvent(session!.id, {
      eventType: payload.eventType ?? 'console.manual',
      severity: payload.severity ?? 'error',
      message: payload.message,
      metadataJson: payload.metadataJson ?? { source: 'code_studio_manual' },
    }),
    onSuccess: (_event, variables) => {
      if (!variables.silent) {
        setManualIssue('');
        message.success(t('rd.previewIssueRecorded', '已记录预览问题'));
      }
      if (session?.id) void queryClient.invalidateQueries({ queryKey: queryKeys.rd.previewLogs(session.id) });
    },
  });

  const recordPreviewEvent = useCallback((payload: { eventType?: string; severity?: string; message?: string; metadataJson?: Record<string, unknown> }) => {
    if (!session?.id || !payload.message?.trim()) return;
    const eventType = payload.eventType ?? 'console';
    const severity = payload.severity ?? 'info';
    const messageText = payload.message.trim();
    setCapturedEvents((previous) => [
      { eventType, severity, message: messageText },
      ...previous,
    ].slice(0, 20));
    consoleMutation.mutate({
      eventType,
      severity,
      message: messageText,
      metadataJson: { ...(payload.metadataJson ?? {}), source: 'code_studio_preview_capture' },
      silent: true,
    });
  }, [consoleMutation, session?.id]);

  useEffect(() => {
    let cancelled = false;
    let refreshTimer: ReturnType<typeof setTimeout> | null = null;
    const sessionId = session?.id;
    if (!sessionId || !session.proxiedUrl) {
      setAuthorizedPreviewUrl('');
      return undefined;
    }

    const authorize = async () => {
      try {
        const authorization = await rdApi.authorizePreviewSession(sessionId);
        if (cancelled) return;
        setAuthorizedPreviewUrl(authorization.url);
        const refreshAfterMs = Math.max(60_000, (authorization.expiresInSeconds - 60) * 1000);
        refreshTimer = setTimeout(() => { void authorize(); }, refreshAfterMs);
      } catch (error) {
        if (!cancelled) {
          setAuthorizedPreviewUrl('');
          message.error((error as Error).message || t('rd.previewAuthorizeFailed', '预览授权失败'));
        }
      }
    };
    void authorize();
    return () => {
      cancelled = true;
      if (refreshTimer) clearTimeout(refreshTimer);
    };
  }, [session?.id, session?.proxiedUrl, t]);

  useEffect(() => {
    function handleMessage(event: MessageEvent) {
      if (event.source !== previewFrameRef.current?.contentWindow) return;
      const data = event.data as { type?: string; sessionId?: string; eventType?: string; severity?: string; message?: string; metadataJson?: Record<string, unknown> } | null;
      if (!data || data.type !== 'aos-preview-event' || data.sessionId !== session?.id) return;
      recordPreviewEvent(data);
    }
    window.addEventListener('message', handleMessage);
    return () => window.removeEventListener('message', handleMessage);
  }, [recordPreviewEvent, session?.id]);

  const currentUrl = authorizedPreviewUrl;
  const running = session?.status === 'running' || session?.status === 'starting';

  if (!repository) {
    return (
      <Empty
        image={Empty.PRESENTED_IMAGE_SIMPLE}
        description={<span style={{ color: '#94a3b8' }}>{t('rd.previewNoRepository', '选择仓库后可以启动前端预览')}</span>}
      />
    );
  }

  return (
    <div className="rd-preview-panel">
      <div className="rd-preview-toolbar">
        <Space wrap size={8} style={{ minWidth: 0 }}>
          <Tag color={previewStatusColor(session?.status)}>{session?.status ?? t('rd.previewNotStarted', '未启动')}</Tag>
          {session?.port ? <Tag>:{session.port}</Tag> : null}
          {session?.runtimeSessionId ? <Tag color="purple">runtime</Tag> : null}
        </Space>
        <Space wrap size={8}>
          <Button
            size="small"
            type="primary"
            icon={<PlayCircleOutlined />}
            disabled={!command.trim() || running}
            loading={startMutation.isPending}
            onClick={() => startMutation.mutate()}
          >
            {t('rd.previewStart', '启动')}
          </Button>
          <Button
            size="small"
            danger
            icon={<StopOutlined />}
            disabled={!session || !running}
            loading={stopMutation.isPending}
            onClick={() => stopMutation.mutate()}
          >
            {t('rd.previewStop', '停止')}
          </Button>
          <Button size="small" icon={<ReloadOutlined />} disabled={!currentUrl} onClick={() => setIframeKey((value) => value + 1)}>
            {t('rd.previewRefresh', '刷新')}
          </Button>
          <Button
            size="small"
            icon={<CameraOutlined />}
            disabled={!session}
            loading={screenshotMutation.isPending}
            onClick={() => screenshotMutation.mutate()}
          >
            {t('rd.previewScreenshot', '截图')}
          </Button>
        </Space>
      </div>
      <Form layout="vertical" className="rd-preview-form">
        <Form.Item label={t('rd.previewCommand', '启动命令')}>
          <Input value={command} onChange={(event) => setCommand(event.target.value)} placeholder="npm run dev" />
        </Form.Item>
        <Space.Compact style={{ width: '100%' }}>
          <InputNumber value={port} min={1} max={65535} onChange={(value) => setPort(typeof value === 'number' ? value : null)} style={{ width: 120 }} />
          <Input value={path} onChange={(event) => setPath(event.target.value)} placeholder="/" />
        </Space.Compact>
      </Form>
      {session?.lastError ? <Alert type="error" showIcon message={session.lastError} /> : null}
      <div className="rd-preview-frame-shell">
        {session?.status === 'starting' ? (
          <div className="rd-preview-frame-loading">
            <Spin />
            <Text style={{ color: '#94a3b8' }}>{t('rd.previewStarting', '预览服务启动中...')}</Text>
          </div>
        ) : currentUrl && session?.status === 'running' ? (
          <iframe
            ref={previewFrameRef}
            key={iframeKey}
            title="AOS Code Studio Preview"
            src={currentUrl}
            sandbox="allow-scripts allow-forms allow-popups"
            referrerPolicy="no-referrer"
          />
        ) : (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description={<span style={{ color: '#94a3b8' }}>{t('rd.previewEmpty', '启动 dev server 后在这里预览页面')}</span>}
          />
        )}
      </div>
      <div className="rd-preview-debug-box">
        <Space.Compact style={{ width: '100%' }}>
          <Input
            value={manualIssue}
            onChange={(event) => setManualIssue(event.target.value)}
            placeholder={t('rd.previewIssuePlaceholder', '粘贴 console error / network failure，或描述页面异常')}
            onPressEnter={() => {
              if (manualIssue.trim() && session) consoleMutation.mutate({ message: manualIssue.trim() });
            }}
          />
          <Button
            icon={<BugOutlined />}
            disabled={!session || !manualIssue.trim()}
            loading={consoleMutation.isPending}
            onClick={() => consoleMutation.mutate({ message: manualIssue.trim() })}
          >
            {t('rd.previewRecordIssue', '记录')}
          </Button>
          <Button
            disabled={!session && !manualIssue.trim()}
            onClick={() => onFixWithAgent?.(previewEvidencePrompt(t('rd.previewFixPromptPrefix', '请根据预览调试证据修复页面问题：'), session, manualIssue, capturedEvents))}
          >
            {t('rd.previewFixWithAgent', '交给 Agent')}
          </Button>
        </Space.Compact>
      </div>
      <PreviewLogsPanel sessionId={session?.id} />
    </div>
  );
}
