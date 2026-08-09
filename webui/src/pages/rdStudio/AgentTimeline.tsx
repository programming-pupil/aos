import { Alert, Button, Card, Collapse, Empty, Space, Spin, Tag, Timeline, Tooltip, Typography } from 'antd';
import dayjs from 'dayjs';
import { useTranslation } from 'react-i18next';
import type { ReactNode } from 'react';
import type { AgentOpsTask, AgentOpsTaskEvent } from '@/api';
import type { RdTaskWorkbenchResponse, RdWorkbenchAgentTask } from '@/types';
import { formatDurationMs } from './reporting';
import type { RdTimelineEvent } from './types';
import { ContextCacheUsage } from './ContextCacheUsage';
import { isStaleOpenRuntimeTool, runtimeToolTargetLabel } from './runtimeTimeline';

const { Text } = Typography;

type AgentOpsBridgeTask =
  | (Pick<AgentOpsTask, 'id' | 'status' | 'phase' | 'progressPercent' | 'lastEvent' | 'errorMessage' | 'runtimeSession'> & Partial<Pick<AgentOpsTask, 'queue' | 'source' | 'externalPlatform'>>)
  | RdWorkbenchAgentTask;

export interface AgentTimelineProps {
  task: AgentOpsBridgeTask | null;
  opsEvents: AgentOpsTaskEvent[];
  workbench: RdTaskWorkbenchResponse | null;
  events: RdTimelineEvent[];
  eventStageGroups: Array<{ stage: string; events: RdTimelineEvent[]; latest?: RdTimelineEvent }>;
  terminalTimelineStatus?: string;
  hasNextPage?: boolean;
  isFetchingNextPage?: boolean;
  renderStatusTag: (value?: string | null) => ReactNode;
  canCancelTask: boolean;
  canRetryTask: boolean;
  cancelLoading?: boolean;
  retryLoading?: boolean;
  onCancel: () => void;
  onRetry: () => void;
  onReviewDiff: () => void;
  onShowTests: () => void;
  onOpenArtifact: (artifact: { sessionId: string; artifactId: string; label: string }) => void;
  onLoadMore: () => void;
}

function traceColor(event: { severity?: string | null; status?: string | null }) {
  if (event.severity === 'error') return 'red';
  if (event.severity === 'warn') return 'orange';
  if (event.status === 'completed') return 'green';
  return 'blue';
}

function statusColor(status?: string | null) {
  if (status === 'failed' || status === 'timed_out') return 'red';
  if (status === 'running') return 'processing';
  return 'default';
}

function AgentOpsTimelineBridge({
  task,
  opsEvents,
  workbench,
  renderStatusTag,
  canCancelTask,
  canRetryTask,
  cancelLoading,
  retryLoading,
  onCancel,
  onRetry,
  onReviewDiff,
  onShowTests,
  onOpenArtifact,
}: Omit<AgentTimelineProps, 'events' | 'eventStageGroups' | 'terminalTimelineStatus' | 'hasNextPage' | 'isFetchingNextPage' | 'onLoadMore'>) {
  const { t } = useTranslation();
  if (!task) return null;
  const active = ['queued', 'claimed', 'running', 'waiting_input', 'retrying'].includes(task.status);
  const taskRuntimeSession = 'runtimeSession' in task ? task.runtimeSession : null;
  const taskQueueStatus = 'queue' in task ? task.queue?.status : null;
  const taskSource = 'source' in task ? task.source : null;
  const taskExternalPlatform = 'externalPlatform' in task ? task.externalPlatform : null;
  const progressPercent = 'progressPercent' in task && typeof task.progressPercent === 'number'
    ? task.progressPercent
    : 0;
  const runtimeSession = workbench?.runtimeSession ?? taskRuntimeSession ?? null;
  const runtimeProcesses = workbench?.runtimeProcesses ?? [];
  const runtimeArtifacts = workbench?.runtimeArtifacts ?? [];
  const traceEvents = workbench?.traceEvents ?? [];
  const suggestedActions = workbench?.suggestedActions ?? [];
  const timelineItems = traceEvents.length > 0
    ? traceEvents.slice(0, 8).map((event) => ({
        color: traceColor(event),
        children: (
          <Space direction="vertical" size={2} style={{ width: '100%' }}>
            <Space size={6} wrap>
              <Text style={{ color: '#e2e8f0' }}>{event.eventType}</Text>
              {event.phase ? <Tag>{event.phase}</Tag> : null}
              {event.status ? renderStatusTag(event.status) : null}
              <Text style={{ color: '#64748b', fontSize: 12 }}>{dayjs(event.createdAt).format('HH:mm:ss')}</Text>
            </Space>
            <Text style={{ color: '#94a3b8', overflowWrap: 'anywhere' }}>{event.message}</Text>
            {(event.durationMs || event.tokenInput || event.tokenOutput || event.runtimeProcessId) ? (
              <Space size={[6, 4]} wrap>
                {event.durationMs ? <Tag color="geekblue">{t('rd.agentTraceDuration', '耗时')}: {formatDurationMs(event.durationMs)}</Tag> : null}
                {(event.tokenInput || event.tokenOutput) ? (
                  <Tag color="purple">{t('rd.agentTraceTokens', 'Token')}: {event.tokenInput ?? 0}/{event.tokenOutput ?? 0}</Tag>
                ) : null}
                {event.runtimeProcessId ? <Tag>{t('rd.agentTraceProcess', '进程')}: {event.runtimeProcessId.slice(0, 8)}</Tag> : null}
              </Space>
            ) : null}
          </Space>
        ),
      }))
    : opsEvents.slice(0, 6).map((event) => ({
        color: traceColor(event),
        children: (
          <Space direction="vertical" size={2} style={{ width: '100%' }}>
            <Space size={6} wrap>
              <Text style={{ color: '#e2e8f0' }}>{event.eventType}</Text>
              {event.phase ? <Tag>{event.phase}</Tag> : null}
              {event.status ? renderStatusTag(event.status) : null}
              <Text style={{ color: '#64748b', fontSize: 12 }}>{dayjs(event.createdAt).format('HH:mm:ss')}</Text>
            </Space>
            <Text style={{ color: '#94a3b8', overflowWrap: 'anywhere' }}>{event.message}</Text>
          </Space>
        ),
      }));
  const currentProcess = runtimeProcesses.find((item) => item.status === 'running' || item.status === 'cancelling')
    ?? runtimeProcesses[0]
    ?? null;
  const currentCommand = currentProcess?.command ?? taskRuntimeSession?.currentCommand ?? null;

  return (
    <Card
      size="small"
      title={
        <Space size={8} wrap>
          <span>{t('rd.agentOpsBridgeTitle', 'AgentOps 观测')}</span>
          {renderStatusTag(task.status)}
          {active ? <Spin size="small" /> : null}
        </Space>
      }
      style={{ marginBottom: 12, background: '#07111f', borderColor: 'rgba(59, 130, 246, 0.28)' }}
    >
      <Space direction="vertical" size={8} style={{ width: '100%' }}>
        <Space size={[6, 6]} wrap>
          <Tag color="blue">{t('rd.agentOpsPhase', '阶段')}: {task.phase || t('common.na')}</Tag>
          <Tag color="cyan">{t('rd.agentOpsProgress', '进度')}: {progressPercent}%</Tag>
          <Tag color="gold">{t('rd.agentOpsQueue', '队列')}: {taskQueueStatus || t('common.na')}</Tag>
          {taskSource ? <Tag color="geekblue">{t('rd.agentOpsSource', '来源')}: {taskSource}</Tag> : null}
          {taskExternalPlatform ? <Tag color="purple">{taskExternalPlatform}</Tag> : null}
        </Space>
        {suggestedActions.length > 0 ? (
          <Space direction="vertical" size={4} style={{ width: '100%' }}>
            <Text style={{ color: '#94a3b8', fontSize: 12 }}>
              {t('rd.agentSuggestedActions', '建议操作')}
            </Text>
            <Space size={[8, 6]} wrap>
              {suggestedActions.slice(0, 4).map((action) => {
                if (action === 'cancel') {
                  return (
                    <Button key={action} size="small" danger disabled={!canCancelTask} loading={cancelLoading} onClick={onCancel}>
                      {t('rd.suggestedCancel', '取消任务')}
                    </Button>
                  );
                }
                if (action === 'retry') {
                  return (
                    <Button key={action} size="small" disabled={!canRetryTask} loading={retryLoading} onClick={onRetry}>
                      {t('rd.suggestedRetry', '重试任务')}
                    </Button>
                  );
                }
                if (action === 'review_diff') {
                  return (
                    <Button key={action} size="small" onClick={onReviewDiff}>
                      {t('rd.suggestedReviewDiff', '审查 Diff')}
                    </Button>
                  );
                }
                if (action === 'fix_tests') {
                  return (
                    <Button key={action} size="small" onClick={onShowTests}>
                      {t('rd.suggestedFixTests', '查看测试')}
                    </Button>
                  );
                }
                return <Tag key={action}>{action}</Tag>;
              })}
            </Space>
          </Space>
        ) : null}
        {runtimeSession ? (
          <Space direction="vertical" size={4} style={{ width: '100%' }}>
            <Space size={[6, 6]} wrap>
              <Tag color={runtimeSession.status === 'running' ? 'processing' : runtimeSession.status === 'failed' ? 'red' : 'default'}>
                {t('rd.agentRuntimeStatus', 'Runtime')}: {runtimeSession.status}
              </Tag>
              <Tag color={runtimeSession.cancelRequested ? 'orange' : 'default'}>
                {runtimeSession.isolationMode}
              </Tag>
              {runtimeSession.heartbeatAt ? (
                <Text style={{ color: '#64748b', fontSize: 12 }}>
                  {t('rd.agentRuntimeHeartbeat', '心跳')}: {dayjs(runtimeSession.heartbeatAt).format('HH:mm:ss')}
                </Text>
              ) : null}
            </Space>
            {currentCommand ? (
              <Tooltip title={currentCommand}>
                <Text
                  style={{
                    color: '#cbd5e1',
                    display: 'block',
                    fontSize: 12,
                    maxWidth: '100%',
                  }}
                  ellipsis
                >
                  {t('rd.agentRuntimeCommand', '当前命令')}: {currentCommand}
                </Text>
              </Tooltip>
            ) : null}
            {runtimeProcesses.length > 0 ? (
              <Space direction="vertical" size={4} style={{ width: '100%' }}>
                <Text style={{ color: '#94a3b8', fontSize: 12 }}>
                  {t('rd.agentRuntimeProcesses', '运行进程')}
                </Text>
                {runtimeProcesses.slice(0, 3).map((process) => {
                  const outputPreview = process.stderrPreview || process.stdoutPreview || '';
                  const startedAt = process.startedAt ? dayjs(process.startedAt) : null;
                  const completedAt = process.completedAt ? dayjs(process.completedAt) : null;
                  const durationMs = startedAt && completedAt
                    ? Math.max(0, completedAt.valueOf() - startedAt.valueOf())
                    : null;
                  return (
                    <div
                      key={process.id}
                      style={{
                        border: '1px solid rgba(148, 163, 184, 0.16)',
                        borderRadius: 6,
                        padding: '6px 8px',
                        background: 'rgba(15, 23, 42, 0.68)',
                      }}
                    >
                      <Space direction="vertical" size={3} style={{ width: '100%' }}>
                        <Space size={[6, 4]} wrap>
                          <Tag color={statusColor(process.status)}>
                            {process.status}
                          </Tag>
                          {typeof process.exitCode === 'number' ? <Tag>exit {process.exitCode}</Tag> : null}
                          {durationMs != null ? <Tag color="geekblue">{formatDurationMs(durationMs)}</Tag> : null}
                        </Space>
                        <Tooltip title={process.command}>
                          <Text
                            code
                            style={{
                              display: 'block',
                              maxWidth: '100%',
                              overflowWrap: 'anywhere',
                              whiteSpace: 'normal',
                            }}
                            ellipsis
                          >
                            {process.command}
                          </Text>
                        </Tooltip>
                        {outputPreview ? (
                          <Typography.Paragraph
                            type={process.stderrPreview ? 'danger' : 'secondary'}
                            ellipsis={{ rows: 2, tooltip: outputPreview }}
                            style={{
                              margin: 0,
                              maxWidth: '100%',
                              overflowWrap: 'anywhere',
                              fontSize: 12,
                            }}
                          >
                            {outputPreview}
                          </Typography.Paragraph>
                        ) : null}
                      </Space>
                    </div>
                  );
                })}
              </Space>
            ) : null}
          </Space>
        ) : null}
        {runtimeSession && runtimeArtifacts.length > 0 ? (
          <Space direction="vertical" size={4} style={{ width: '100%' }}>
            <Text style={{ color: '#94a3b8', fontSize: 12 }}>
              {t('rd.agentRuntimeArtifacts', '运行产物')}
            </Text>
            <Space size={[6, 6]} wrap>
              {runtimeArtifacts.slice(0, 6).map((artifact) => (
                <Tooltip key={artifact.id} title={artifact.path || artifact.id}>
                  <Button
                    size="small"
                    type="link"
                    style={{
                      maxWidth: 240,
                      paddingInline: 0,
                      color: '#93c5fd',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                    }}
                    onClick={() => onOpenArtifact({
                      sessionId: runtimeSession.id,
                      artifactId: artifact.id,
                      label: artifact.path || artifact.id,
                    })}
                  >
                    {artifact.artifactType} · {artifact.sizeBytes}B
                  </Button>
                </Tooltip>
              ))}
            </Space>
          </Space>
        ) : null}
        {task.lastEvent ? <Text style={{ color: '#cbd5e1' }}>{task.lastEvent}</Text> : null}
        {task.errorMessage ? <Alert type="error" showIcon message={task.errorMessage} /> : null}
        {timelineItems.length > 0 ? (
          <Space direction="vertical" size={4} style={{ width: '100%' }}>
            <Text style={{ color: '#94a3b8', fontSize: 12 }}>
              {traceEvents.length > 0 ? t('rd.agentTraceTimeline', 'Agent Trace') : t('rd.agentOpsEventTimeline', 'AgentOps 事件')}
            </Text>
            <Timeline style={{ marginTop: 4 }} items={timelineItems} />
          </Space>
        ) : (
          <Text style={{ color: '#64748b' }}>{t('rd.agentOpsNoEvents', '暂无 AgentOps 事件')}</Text>
        )}
      </Space>
    </Card>
  );
}

export function AgentTimeline({
  task,
  opsEvents,
  workbench,
  events,
  eventStageGroups,
  terminalTimelineStatus,
  hasNextPage,
  isFetchingNextPage,
  renderStatusTag,
  canCancelTask,
  canRetryTask,
  cancelLoading,
  retryLoading,
  onCancel,
  onRetry,
  onReviewDiff,
  onShowTests,
  onOpenArtifact,
  onLoadMore,
}: AgentTimelineProps) {
  const { t } = useTranslation();
  const bridge = (
    <AgentOpsTimelineBridge
      task={task}
      opsEvents={opsEvents}
      workbench={workbench}
      renderStatusTag={renderStatusTag}
      canCancelTask={canCancelTask}
      canRetryTask={canRetryTask}
      cancelLoading={cancelLoading}
      retryLoading={retryLoading}
      onCancel={onCancel}
      onRetry={onRetry}
      onReviewDiff={onReviewDiff}
      onShowTests={onShowTests}
      onOpenArtifact={onOpenArtifact}
    />
  );

  if (events.length === 0) {
    return (
      <>
        {bridge}
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.noEvents', '暂无事件')}</span>} />
      </>
    );
  }

  return (
    <>
      {bridge}
      <div
        style={{ maxHeight: 520, overflowY: 'auto', paddingRight: 6 }}
        onScroll={(event) => {
          const target = event.currentTarget;
          const nearBottom = target.scrollTop + target.clientHeight >= target.scrollHeight - 32;
          if (nearBottom && hasNextPage && !isFetchingNextPage) {
            onLoadMore();
          }
        }}
      >
        <Collapse
          className="rd-timeline-stage-collapse"
          defaultActiveKey={eventStageGroups.slice(0, 2).map((group) => group.stage)}
          items={eventStageGroups.map((group) => {
            const latestStale = group.latest ? isStaleOpenRuntimeTool(group.latest) : false;
            const latestStatus = group.latest?.status === 'running'
              ? terminalTimelineStatus ?? (latestStale ? 'stale' : group.latest.status)
              : group.latest?.status;
            return {
              key: group.stage,
              label: (
                <Space size={8} wrap>
                  <Text style={{ color: '#e2e8f0' }}>{group.stage}</Text>
                  {renderStatusTag(latestStatus)}
                  <Tag color="blue">{t('rd.eventCount', '{{count}} 条', { count: group.events.length })}</Tag>
                </Space>
              ),
              children: (
                <Timeline
                  items={group.events.map((event) => {
                    const staleRuntimeTool = isStaleOpenRuntimeTool(event);
                    const displayStatus = event.status === 'running'
                      ? terminalTimelineStatus ?? (staleRuntimeTool ? 'stale' : event.status)
                      : event.status;
                    const target = event.displayToolTarget ?? runtimeToolTargetLabel(event);
                    const startedAt = event.displayStartedAt;
                    const completedAt = event.displayCompletedAt ?? (displayStatus === 'running' || staleRuntimeTool ? undefined : event.createdAt);
                    const runningElapsedMs = startedAt && displayStatus === 'running'
                      ? Math.max(0, Date.now() - dayjs(startedAt).valueOf())
                      : undefined;
                    const staleElapsedMs = startedAt && staleRuntimeTool
                      ? Math.max(0, Date.now() - dayjs(startedAt).valueOf())
                      : undefined;
                    const durationMs = event.displayDurationMs ?? runningElapsedMs ?? staleElapsedMs;
                    return {
                      color: displayStatus === 'failed' || displayStatus === 'timeout'
                        ? 'red'
                        : displayStatus === 'stale'
                          ? 'orange'
                          : displayStatus === 'running'
                            ? 'blue'
                            : displayStatus === 'waiting_approval'
                              ? 'orange'
                              : 'green',
                      children: (
                        <div>
                          <Space size={6} wrap>
                            {displayStatus === 'stale'
                              ? renderStatusTag('stale')
                              : renderStatusTag(displayStatus)}
                            {durationMs != null ? (
                              <Tag color="geekblue">{t('rd.toolDuration', '耗时')}: {formatDurationMs(durationMs)}</Tag>
                            ) : null}
                          </Space>
                          <div style={{ color: staleRuntimeTool ? '#fbbf24' : '#94a3b8', fontSize: 12, marginTop: 2 }}>
                            {event.message}
                            {staleRuntimeTool
                              ? `（${t('rd.runtimeToolStaleHint', '长时间未收到工具结果，可能是 runtime 进程中断或任务未被正确回收')}）`
                              : ''}
                          </div>
                          {target ? (
                            <div style={{ color: '#cbd5e1', fontSize: 12, marginTop: 2, wordBreak: 'break-all' }}>
                              {t('rd.toolTarget', '目标')}: {target}
                            </div>
                          ) : null}
                          <ContextCacheUsage event={event} />
                          <Space size={8} wrap style={{ marginTop: 2 }}>
                            {startedAt ? (
                              <Text style={{ color: '#64748b', fontSize: 11 }}>
                                {t('rd.toolStartedAt', '开始')}: {dayjs(startedAt).format('YYYY-MM-DD HH:mm:ss')}
                              </Text>
                            ) : null}
                            <Text style={{ color: '#64748b', fontSize: 11 }}>
                              {staleRuntimeTool
                                ? `${t('rd.toolLastSeenAt', '最后事件')}: ${dayjs(event.createdAt).format('YYYY-MM-DD HH:mm:ss')}`
                                : displayStatus === 'running'
                                  ? `${t('rd.toolCurrentAt', '当前')}: ${dayjs().format('YYYY-MM-DD HH:mm:ss')}`
                                  : `${t('rd.toolCompletedAt', '完成')}: ${dayjs(completedAt ?? event.createdAt).format('YYYY-MM-DD HH:mm:ss')}`}
                            </Text>
                          </Space>
                        </div>
                      ),
                    };
                  })}
                />
              ),
            };
          })}
        />
        <div style={{ textAlign: 'center', padding: '4px 0 8px' }}>
          {isFetchingNextPage ? (
            <Spin size="small" />
          ) : hasNextPage ? (
            <Button type="link" size="small" onClick={onLoadMore}>
              {t('rd.scrollToLoadMoreEvents', '下滑加载更早事件')}
            </Button>
          ) : null}
        </div>
      </div>
    </>
  );
}
