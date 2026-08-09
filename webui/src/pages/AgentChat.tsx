// ── AgentChat — R&D AI development page with Git project binding & pipeline ─────────
//
// Wraps ChatCore with agent-specific features:
//   • 8-stage development pipeline (discover → deploy)
//   • GitLab project binding as development context
//   • Pipeline logs & stage advancement
//
// Pipeline stage advancement is currently frontend-guessed from tool names.
// Future: backend should emit semantic pipeline events via SSE.

import { useState, useRef, useCallback, useEffect, useMemo } from 'react';
import {
  Layout,
  Typography,
  Button,
  Input,
  Space,
  message,
  Tag,
  Tooltip,
  Badge,
  Drawer,
  Empty,
  Spin,
} from 'antd';
import {
  SendOutlined,
  PaperClipOutlined,
  Loading3QuartersOutlined,
  LoadingOutlined,
  FolderOpenOutlined,
  CheckCircleOutlined,
  CloseCircleOutlined,
  SearchOutlined,
  RobotOutlined,
  RocketOutlined,
  ProjectOutlined,
  PlayCircleOutlined,
  StopOutlined,
  SyncOutlined,
  CodeOutlined,
  CloudOutlined,
} from '@ant-design/icons';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { agentApi, projectsApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { ToolCallInfo } from '@/components/chat/types';
import type { GitlabProject } from '@/types';
import { ChatCore, DisplayMessage } from '@/components/chat/ChatCore';
import type { ContentBlock } from '@/types';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import { useTranslation } from 'react-i18next';

dayjs.extend(relativeTime);

const { Text, Title } = Typography;

// ─── Pipeline types ───────────────────────────────────────────────────────────

interface PipelineStage {
  id: string;
  name: string;
  status: 'pending' | 'running' | 'success' | 'failed' | 'skipped';
  icon: React.ReactNode;
  steps: PipelineStep[];
  duration?: number;
  logs: Array<{
    time: string;
    level: 'info' | 'success' | 'warn' | 'error';
    msg: string;
  }>;
}

interface PipelineStep {
  id: string;
  name: string;
  status: 'pending' | 'running' | 'success' | 'failed' | 'skipped';
  duration?: number;
  output?: string;
}

type PipelineViewTab = 'logs' | 'diff' | 'usage' | 'review';

// ─── Pipeline stage definitions ─────────────────────────────────────────────

const PIPELINE_STAGES: Array<{
  id: string;
  labelKey: string;
  icon: React.ReactNode;
  color: string;
}> = [
  {
    id: 'discover',
    labelKey: 'agent.stage.discover',
    icon: <SearchOutlined style={{ fontSize: 14 }} />,
    color: '#a855f7',
  },
  {
    id: 'analyze',
    labelKey: 'agent.stage.analyze',
    icon: <ProjectOutlined style={{ fontSize: 14 }} />,
    color: '#f97316',
  },
  {
    id: 'plan',
    labelKey: 'agent.stage.plan',
    icon: <CodeOutlined style={{ fontSize: 14 }} />,
    color: '#3b82f6',
  },
  {
    id: 'write',
    labelKey: 'agent.stage.write',
    icon: <CodeOutlined style={{ fontSize: 14 }} />,
    color: '#22c55e',
  },
  {
    id: 'build',
    labelKey: 'agent.stage.build',
    icon: <CloudOutlined style={{ fontSize: 14 }} />,
    color: '#06b6d4',
  },
  {
    id: 'test',
    labelKey: 'agent.stage.test',
    icon: <PlayCircleOutlined style={{ fontSize: 14 }} />,
    color: '#f59e0b',
  },
  {
    id: 'commit',
    labelKey: 'agent.stage.commit',
    icon: <SyncOutlined style={{ fontSize: 14 }} />,
    color: '#8b5cf6',
  },
  {
    id: 'deploy',
    labelKey: 'agent.stage.deploy',
    icon: <RocketOutlined style={{ fontSize: 14 }} />,
    color: '#ec4899',
  },
];

// ─── Stage indicator ─────────────────────────────────────────────────────────

function StageIndicator({
  activeStage,
  status,
  onClick,
}: {
  activeStage: string;
  status: Record<string, 'pending' | 'running' | 'success' | 'failed' | 'skipped'>;
  onClick?: (stageId: string) => void;
}) {
  const { t } = useTranslation();
  const activeIndex = PIPELINE_STAGES.findIndex((s) => s.id === activeStage);

  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 0,
        padding: '6px 12px',
        background: 'var(--bg-elevated)',
        borderBottom: '1px solid var(--border-subtle)',
        overflowX: 'auto',
        flexShrink: 0,
      }}
    >
      {PIPELINE_STAGES.map((stage, idx) => {
        const stageStatus = status[stage.id] ?? 'pending';
        const isActive = stage.id === activeStage;
        const isPast = activeIndex > idx || stageStatus === 'success';
        const isFailed = stageStatus === 'failed';
        const isRunning = stageStatus === 'running';
        const iconColor = isFailed
          ? 'var(--color-error)'
          : isRunning
            ? stage.color
            : isPast
              ? 'var(--color-success)'
              : 'var(--text-muted)';

        return (
          <div key={stage.id} style={{ display: 'flex', alignItems: 'center' }}>
            {idx > 0 && (
              <div
                style={{
                  width: 20,
                  height: 1,
                  background:
                    isPast && !isFailed ? stage.color : 'var(--border-default)',
                  flexShrink: 0,
                  transition: 'background 0.3s',
                }}
              />
            )}
            <Tooltip title={`${t(stage.labelKey)} · ${stageStatus}`}>
              <div
                onClick={() => onClick?.(stage.id)}
                style={{
                  display: 'flex',
                  flexDirection: 'column',
                  alignItems: 'center',
                  gap: 2,
                  padding: '4px 8px',
                  borderRadius: 8,
                  cursor: onClick ? 'pointer' : 'default',
                  background: isActive ? `${stage.color}18` : 'transparent',
                  border: `1px solid ${isActive ? stage.color + '60' : 'transparent'}`,
                  transition: 'all 0.2s',
                  minWidth: 52,
                }}
              >
                <div style={{ color: iconColor, display: 'flex', alignItems: 'center' }}>
                  {isRunning ? (
                    <LoadingOutlined spin style={{ fontSize: 14 }} />
                  ) : isFailed ? (
                    <CloseCircleOutlined style={{ fontSize: 14 }} />
                  ) : isPast ? (
                    <CheckCircleOutlined style={{ fontSize: 14 }} />
                  ) : (
                    stage.icon
                  )}
                </div>
                <Text
                  style={{
                    fontSize: 9,
                    color: isActive ? stage.color : 'var(--text-muted)',
                    textAlign: 'center',
                    lineHeight: 1.2,
                    whiteSpace: 'nowrap',
                  }}
                >
                  {t(stage.labelKey)}
                </Text>
              </div>
            </Tooltip>
          </div>
        );
      })}
    </div>
  );
}

// ─── Pipeline panel ─────────────────────────────────────────────────────────

function PipelinePanel({
  stages,
  logs,
  isRunning,
  onAbort,
  usage,
}: {
  stages: PipelineStage[];
  logs: Array<{
    time: string;
    level: 'info' | 'success' | 'warn' | 'error';
    msg: string;
  }>;
  isRunning?: boolean;
  onAbort?: () => void;
  usage?: { inputTokens: number; outputTokens: number; estimatedCostUsd?: number } | null;
}) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<PipelineViewTab>('logs');
  const [selectedStageId, setSelectedStageId] = useState<string | null>(null);
  const logsEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    logsEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [logs.length]);

  const selectedStage =
    stages.find((s) => s.id === selectedStageId) ??
    stages.find((s) => s.status === 'running') ??
    stages[0];

  const overallStatus = stages.some((s) => s.status === 'failed')
    ? 'failed'
    : stages.every(
        (s) => s.status === 'success' || s.status === 'skipped' || s.status === 'pending',
      )
      ? 'success'
      : 'running';

  const statusColors: Record<string, string> = {
    success: 'var(--color-success)',
    failed: 'var(--color-error)',
    running: 'var(--accent-ai)',
    pending: 'var(--text-muted)',
    skipped: 'var(--text-muted)',
  };

  const logColors: Record<string, string> = {
    info: 'var(--text-secondary)',
    success: 'var(--color-success)',
    warn: 'var(--color-warning)',
    error: 'var(--color-error)',
  };

  const logIcons: Record<string, string> = {
    info: '▶',
    success: '✓',
    warn: '⚠',
    error: '✗',
  };

  return (
    <div
      style={{
        height: '100%',
        display: 'flex',
        flexDirection: 'column',
        overflow: 'hidden',
        background: 'var(--bg-surface)',
      }}
    >
      {/* Header */}
      <div
        style={{
          padding: '10px 16px',
          borderBottom: '1px solid var(--border-subtle)',
          display: 'flex',
          alignItems: 'center',
          gap: 10,
          flexShrink: 0,
        }}
      >
        <span style={{ fontSize: 16 }}>🚀</span>
        <Title level={5} style={{ margin: 0, fontSize: 14, color: 'var(--text-primary)' }}>
          {t('agent.pipeline')}
        </Title>
        <Tag
          color={
            overallStatus === 'success'
              ? 'success'
              : overallStatus === 'failed'
                ? 'error'
                : 'processing'
          }
          style={{ fontSize: 11 }}
        >
          {overallStatus === 'success'
            ? t('agent.completed')
            : overallStatus === 'failed'
              ? t('agent.failed')
              : t('agent.running')}
        </Tag>
        {isRunning && (
          <Tag color="purple" style={{ fontSize: 11, marginLeft: 'auto' }}>
            <LoadingOutlined spin style={{ marginRight: 4 }} />
            {t('agent.working')}
          </Tag>
        )}
        {onAbort && isRunning && (
          <Button
            size="small"
            danger
            icon={<StopOutlined />}
            onClick={onAbort}
            style={{ marginLeft: 'auto' }}
          >
            {t('pipeline.abort')}
          </Button>
        )}
      </div>

      {/* Stage strip */}
      <div
        style={{
          display: 'flex',
          overflowX: 'auto',
          padding: '8px 12px',
          gap: 4,
          borderBottom: '1px solid var(--border-subtle)',
          flexShrink: 0,
        }}
      >
        {stages.map((stage) => (
          <Tooltip key={stage.id} title={`${stage.name} · ${stage.status}`}>
            <div
              onClick={() =>
                setSelectedStageId(
                  stage.id === selectedStageId ? null : stage.id,
                )
              }
              style={{
                display: 'flex',
                flexDirection: 'column',
                alignItems: 'center',
                gap: 3,
                padding: '6px 10px',
                borderRadius: 8,
                cursor: 'pointer',
                border: `1px solid ${
                  stage.id === selectedStageId
                    ? 'var(--accent-ai)'
                    : 'transparent'
                }`,
                background:
                  stage.id === selectedStageId
                    ? 'rgba(124,58,237,0.1)'
                    : 'transparent',
                transition: 'all 0.15s',
                minWidth: 60,
              }}
            >
              <div style={{ color: statusColors[stage.status] }}>
                {stage.status === 'running' ? (
                  <LoadingOutlined spin style={{ fontSize: 14 }} />
                ) : stage.status === 'success' ? (
                  <CheckCircleOutlined style={{ fontSize: 14 }} />
                ) : stage.status === 'failed' ? (
                  <CloseCircleOutlined style={{ fontSize: 14 }} />
                ) : (
                  <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>○</span>
                )}
              </div>
              <Text
                style={{
                  fontSize: 9,
                  color: statusColors[stage.status],
                  textAlign: 'center',
                  whiteSpace: 'nowrap',
                }}
              >
                {stage.name}
              </Text>
              {stage.duration && (
                <Text style={{ fontSize: 8, color: 'var(--text-muted)' }}>
                  {stage.duration}s
                </Text>
              )}
            </div>
          </Tooltip>
        ))}
      </div>

      {/* Tabs */}
      <div
        style={{
          display: 'flex',
          gap: 0,
          padding: '0 16px',
          borderBottom: '1px solid var(--border-subtle)',
          flexShrink: 0,
        }}
      >
        {(
          [
            { key: 'logs', icon: '📋', label: t('pipeline.tabs.logs') },
            { key: 'diff', icon: '📝', label: t('pipeline.tabs.diff') },
            { key: 'usage', icon: '📊', label: t('pipeline.tabs.usage') },
            { key: 'review', icon: '🔍', label: t('pipeline.tabs.review') },
          ] as const
        ).map((tab) => (
          <div
            key={tab.key}
            onClick={() => setActiveTab(tab.key)}
            style={{
              padding: '8px 14px',
              cursor: 'pointer',
              borderBottom: `2px solid ${
                activeTab === tab.key ? 'var(--accent-ai)' : 'transparent'
              }`,
              color:
                activeTab === tab.key
                  ? 'var(--text-primary)'
                  : 'var(--text-muted)',
              fontSize: 12,
              fontWeight: activeTab === tab.key ? 600 : 400,
              display: 'flex',
              alignItems: 'center',
              gap: 5,
              transition: 'all 0.15s',
              marginBottom: -1,
            }}
          >
            <span>{tab.icon}</span>
            {tab.label}
          </div>
        ))}
      </div>

      {/* Content */}
      <div style={{ flex: 1, overflow: 'auto', display: 'flex' }}>
        {/* Stage steps sidebar */}
        {selectedStage && (
          <div
            style={{
              width: 220,
              borderRight: '1px solid var(--border-subtle)',
              padding: '10px 12px',
              flexShrink: 0,
              overflow: 'auto',
            }}
          >
            <Text
              style={{
                fontSize: 11,
                color: 'var(--text-muted)',
                textTransform: 'uppercase',
                letterSpacing: '0.05em',
              }}
            >
              {selectedStage.name} — {t('pipeline.stageSteps')}
            </Text>
            <div
              style={{ marginTop: 8, display: 'flex', flexDirection: 'column', gap: 4 }}
            >
              {selectedStage.steps.map((step) => (
                <div
                  key={step.id}
                  style={{
                    display: 'flex',
                    alignItems: 'center',
                    gap: 8,
                    padding: '4px 0',
                    borderBottom: '1px solid var(--border-subtle)',
                  }}
                >
                  <span style={{ color: statusColors[step.status], width: 14 }}>
                    {step.status === 'running'
                      ? '◎'
                      : step.status === 'success'
                        ? '✓'
                        : step.status === 'failed'
                          ? '✗'
                          : '○'}
                  </span>
                  <Text
                    style={{
                      fontSize: 11,
                      color: 'var(--text-secondary)',
                      flex: 1,
                      fontFamily: 'var(--font-code)',
                    }}
                    ellipsis
                  >
                    {step.name}
                  </Text>
                  {step.duration != null && (
                    <Text style={{ fontSize: 10, color: 'var(--text-muted)' }}>
                      {step.duration}s
                    </Text>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}

        {/* Tab content */}
        <div style={{ flex: 1, overflow: 'auto', padding: '10px 14px' }}>
          {activeTab === 'logs' && (
            <div
              style={{
                fontFamily: 'var(--font-code)',
                fontSize: 12,
                background: 'var(--bg-void)',
                borderRadius: 6,
                padding: '6px 10px',
                minHeight: 200,
              }}
            >
              {logs.length === 0 && !isRunning ? (
                <Text
                  type="secondary"
                  style={{ fontSize: 12, fontFamily: 'var(--font-ui)' }}
                >
                  {t('agent.noLogs')}
                </Text>
              ) : (
                <>
                  {logs.map((log, i) => (
                    <div
                      key={i}
                      style={{ display: 'flex', gap: 8, padding: '1px 0', color: logColors[log.level] }}
                    >
                      <span
                        style={{
                          color: 'var(--text-muted)',
                          fontSize: 10,
                          flexShrink: 0,
                        }}
                      >
                        {log.time}
                      </span>
                      <span style={{ flexShrink: 0 }}>{logIcons[log.level]}</span>
                      <span>{log.msg}</span>
                    </div>
                  ))}
                  {isRunning && (
                    <div style={{ color: 'var(--accent-ai)', display: 'flex', gap: 8 }}>
                      <LoadingOutlined spin style={{ fontSize: 11 }} />
                      <span>{t('pipeline.agentWorking')}</span>
                    </div>
                  )}
                  <div ref={logsEndRef} />
                </>
              )}
            </div>
          )}

          {activeTab === 'diff' && (
            <div
              style={{
                textAlign: 'center',
                padding: '32px 0',
                color: 'var(--text-muted)',
                fontSize: 13,
              }}
            >
              <span style={{ fontSize: 28, display: 'block', marginBottom: 8 }}>📝</span>
              {t('agent.diffUnavail')}
            </div>
          )}

          {activeTab === 'usage' && (
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 10 }}>
              {[
                {
                  label: t('pipeline.usage.inputTokens'),
                  value: usage ? usage.inputTokens.toLocaleString() : '—',
                  color: 'var(--color-info)',
                },
                {
                  label: t('pipeline.usage.outputTokens'),
                  value: usage ? usage.outputTokens.toLocaleString() : '—',
                  color: 'var(--color-success)',
                },
                {
                  label: t('pipeline.usage.cost'),
                  value: usage?.estimatedCostUsd != null
                    ? `$${usage.estimatedCostUsd.toFixed(4)}`
                    : '$0.00',
                  color: 'var(--accent-ai)',
                },
              ].map((stat) => (
                <div
                  key={stat.label}
                  style={{
                    padding: '10px 12px',
                    background: 'var(--bg-elevated)',
                    borderRadius: 8,
                    border: '1px solid var(--border-subtle)',
                    textAlign: 'center',
                  }}
                >
                  <div style={{ fontSize: 18, fontWeight: 700, color: stat.color }}>
                    {stat.value}
                  </div>
                  <div style={{ fontSize: 10, color: 'var(--text-muted)', marginTop: 2 }}>
                    {stat.label}
                  </div>
                </div>
              ))}
            </div>
          )}

          {activeTab === 'review' && (
            <div
              style={{
                textAlign: 'center',
                padding: '32px 0',
                color: 'var(--text-muted)',
                fontSize: 13,
              }}
            >
              <span style={{ fontSize: 28, display: 'block', marginBottom: 8 }}>🔍</span>
              {t('agent.reviewUnavail')}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// ─── Project selector drawer ────────────────────────────────────────────────

interface ProjectSelectorProps {
  open: boolean;
  onClose: () => void;
  projects: GitlabProject[];
  selectedProjectIds: string[];
  onToggle: (projectId: string) => void;
  onSelectAll: () => void;
  onDeselectAll: () => void;
  loading?: boolean;
}

function ProjectSelector({
  open,
  onClose,
  projects,
  selectedProjectIds,
  onToggle,
  onSelectAll,
  onDeselectAll,
  loading,
}: ProjectSelectorProps) {
  const { t } = useTranslation();
  const [search, setSearch] = useState('');

  const filtered = useMemo(() => {
    if (!search.trim()) return projects;
    const q = search.toLowerCase();
    return projects.filter(
      (p) =>
        p.name?.toLowerCase().includes(q) ||
        p.url?.toLowerCase().includes(q) ||
        p.description?.toLowerCase().includes(q),
    );
  }, [projects, search]);

  return (
    <Drawer
      title={
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <FolderOpenOutlined style={{ color: 'var(--accent-ai)' }} />
          <span>{t('agent.selectProjects')}</span>
          {selectedProjectIds.length > 0 && (
            <Tag color="purple" style={{ fontSize: 11 }}>
              {selectedProjectIds.length} {t('agent.selected')}
            </Tag>
          )}
        </div>
      }
      placement="right"
      onClose={onClose}
      open={open}
      width={380}
      extra={
        <Space size={4}>
          <Button size="small" onClick={onSelectAll} style={{ fontSize: 11 }}>
            {t('agent.selectAll')}
          </Button>
          <Button size="small" onClick={onDeselectAll} style={{ fontSize: 11 }}>
            {t('agent.clear')}
          </Button>
        </Space>
      }
    >
      <Input
        prefix={<SearchOutlined style={{ fontSize: 12 }} />}
        placeholder={t('agent.searchProjects')}
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ marginBottom: 12, fontSize: 13 }}
        allowClear
      />
      {loading ? (
        <div style={{ textAlign: 'center', padding: 32 }}>
          <Spin size="small" />
          <div
            style={{ marginTop: 8, fontSize: 12, color: 'var(--text-muted)' }}
          >
            {t('common.loading')}...
          </div>
        </div>
      ) : filtered.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={t('agent.noProjects')}
          style={{ marginTop: 32 }}
        />
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
          {filtered.map((project) => {
            const isSelected = selectedProjectIds.includes(project.id);
            return (
              <div
                key={project.id}
                onClick={() => onToggle(project.id)}
                style={{
                  padding: '10px 12px',
                  borderRadius: 8,
                  cursor: 'pointer',
                  background: isSelected
                    ? 'rgba(124,58,237,0.08)'
                    : 'var(--bg-elevated)',
                  border: `1px solid ${
                    isSelected ? 'var(--accent-ai)' : 'var(--border-default)'
                  }`,
                  transition: 'all 0.15s',
                  display: 'flex',
                  gap: 10,
                  alignItems: 'flex-start',
                }}
              >
                <div
                  style={{
                    width: 18,
                    height: 18,
                    borderRadius: 4,
                    border: `2px solid ${
                      isSelected ? 'var(--accent-ai)' : 'var(--border-default)'
                    }`,
                    background: isSelected ? 'var(--accent-ai)' : 'transparent',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    flexShrink: 0,
                    marginTop: 2,
                    transition: 'all 0.15s',
                  }}
                >
                  {isSelected && (
                    <span style={{ color: '#fff', fontSize: 10 }}>✓</span>
                  )}
                </div>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div
                    style={{ display: 'flex', alignItems: 'center', gap: 6 }}
                  >
                    <Text
                      style={{ fontSize: 13, fontWeight: 600, color: 'var(--text-primary)' }}
                      ellipsis
                    >
                      {project.name}
                    </Text>
                    {project.branch && (
                      <Tag style={{ fontSize: 10 }} color="default">
                        {project.branch}
                      </Tag>
                    )}
                  </div>
                  <Text
                    type="secondary"
                    style={{ fontSize: 11 }}
                    ellipsis
                  >
                    {project.url ?? project.id}
                  </Text>
                  {project.description && (
                    <Text
                      type="secondary"
                      style={{ fontSize: 11, display: 'block', marginTop: 2 }}
                      ellipsis
                    >
                      {project.description}
                    </Text>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </Drawer>
  );
}

// ─── Main component ─────────────────────────────────────────────────────────

export default function AgentChat() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  // ── Agent-specific state ────────────────────────────────────────────────
  const [selectedProjectIds, setSelectedProjectIds] = useState<string[]>([]);
  const [projectDrawerOpen, setProjectDrawerOpen] = useState(false);
  const [pipelineOpen, setPipelineOpen] = useState(true);
  const [activeStage, setActiveStage] = useState('discover');
  const [pipelineStages, setPipelineStages] = useState<PipelineStage[]>([]);
  const [pipelineLogs, setPipelineLogs] = useState<
    Array<{ time: string; level: 'info' | 'success' | 'warn' | 'error'; msg: string }>
  >([]);
  const [stageStatus, setStageStatus] = useState<
    Record<string, 'pending' | 'running' | 'success' | 'failed' | 'skipped'>
  >({});
  const [usage, setUsage] = useState<{
    inputTokens: number;
    outputTokens: number;
    estimatedCostUsd?: number;
  } | null>(null);
  const [isStreaming, setIsStreaming] = useState(false);

  const activeStageRef = useRef('discover');
  const pipelineOpenRef = useRef(true);
  const abortRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    pipelineOpenRef.current = pipelineOpen;
  }, [pipelineOpen]);
  useEffect(() => {
    activeStageRef.current = activeStage;
  }, [activeStage]);

  // ── Queries ──────────────────────────────────────────────────────────
  const { data: projectsData, isLoading: projectsLoading } = useQuery({
    queryKey: queryKeys.projects?.all ?? ['projects', 'all'],
    queryFn: () => projectsApi.list(),
    staleTime: 60_000,
  });

  const projects: GitlabProject[] = (projectsData?.projects ?? []) as GitlabProject[];

  // ── Pipeline initialization ────────────────────────────────────────────
  const initPipeline = useCallback(() => {
    const stages: PipelineStage[] = PIPELINE_STAGES.map((s) => ({
      id: s.id,
      name: t(s.labelKey),
      status: 'pending',
      icon: s.icon,
      steps: [],
      logs: [],
    }));
    setPipelineStages(stages);
    setStageStatus({});
    setPipelineLogs([]);
  }, [t]);

  useEffect(() => {
    initPipeline();
  }, [initPipeline]);

  // ── Project selection ────────────────────────────────────────────────
  const handleProjectToggle = useCallback((projectId: string) => {
    setSelectedProjectIds((prev) =>
      prev.includes(projectId)
        ? prev.filter((id) => id !== projectId)
        : [...prev, projectId],
    );
  }, []);

  const handleSelectAllProjects = useCallback(
    () => setSelectedProjectIds(projects.map((p) => p.id)),
    [projects],
  );

  const handleDeselectAllProjects = useCallback(
    () => setSelectedProjectIds([]),
    [],
  );

  // ── Pipeline helpers ─────────────────────────────────────────────────
  const addLog = useCallback(
    (
      level: 'info' | 'success' | 'warn' | 'error',
      msg: string,
    ) => {
      setPipelineLogs((prev) => [
        ...prev,
        { time: dayjs().format('HH:mm:ss'), level, msg },
      ]);
    },
    [],
  );

  const advanceStage = useCallback(
    (
      stageId: string,
      status: 'success' | 'failed' = 'success',
    ) => {
      setStageStatus((prev) => ({ ...prev, [stageId]: status }));
      const stageIndex = PIPELINE_STAGES.findIndex((s) => s.id === stageId);
      if (stageIndex < PIPELINE_STAGES.length - 1 && status === 'success') {
        const next = PIPELINE_STAGES[stageIndex + 1].id;
        setActiveStage(next);
        setStageStatus((prev) => ({ ...prev, [next]: 'running' }));
      } else if (status === 'failed') {
        setActiveStage(stageId);
      }
    },
    [],
  );

  const handleStop = useCallback(() => {
    abortRef.current?.();
    setStageStatus((prev) => ({ ...prev, [activeStageRef.current]: 'skipped' }));
  }, []);

  // ── Top bar extras ───────────────────────────────────────────────────
  const topBarExtra = (
    <>
      <Button
        size="small"
        icon={<FolderOpenOutlined />}
        onClick={() => setProjectDrawerOpen(true)}
        style={{ fontSize: 11 }}
      >
        {t('agent.bindProjects')}
        {selectedProjectIds.length > 0 && (
          <Tag color="purple" style={{ fontSize: 10, marginLeft: 4 }}>
            {selectedProjectIds.length}
          </Tag>
        )}
      </Button>

      {selectedProjectIds.length > 0 && (
        <Space size={4} wrap>
          {selectedProjectIds.slice(0, 3).map((pid) => {
            const proj = projects.find((p) => p.id === pid);
            return (
              <Tag key={pid} color="purple" style={{ fontSize: 10 }}>
                📁 {proj?.name ?? pid.slice(0, 8)}
              </Tag>
            );
          })}
          {selectedProjectIds.length > 3 && (
            <Tag style={{ fontSize: 10 }}>
              +{selectedProjectIds.length - 3}
            </Tag>
          )}
        </Space>
      )}
    </>
  );

  const topBarActions = (
    <Button
      size="small"
      icon={<RocketOutlined />}
      onClick={() => setPipelineOpen((v) => !v)}
      style={{
        fontSize: 11,
        background: pipelineOpen
          ? 'rgba(124,58,237,0.12)'
          : 'transparent',
        borderColor: pipelineOpen
          ? 'var(--accent-ai)'
          : 'var(--border-default)',
        color: pipelineOpen ? 'var(--accent-ai)' : 'var(--text-muted)',
      }}
    >
      {t('agent.pipeline')}
    </Button>
  );

  // ─── Render ───────────────────────────────────────────────────────────
  return (
    <>
      <ChatCore
        sessionSource="agent"
        emptySessionText={t('agent.noSession')}
        noSessionPlaceholder={{
          title: t('agent.welcomeTitle'),
          description: t('agent.welcomeDesc'),
          emoji: '🤖',
        }}
        topBarExtra={topBarExtra}
        topBarActions={topBarActions}
        rightPanel={
          <PipelinePanel
            stages={pipelineStages}
            logs={pipelineLogs}
            isRunning={isStreaming}
            onAbort={handleStop}
            usage={usage}
          />
        }
        onStreamingChange={(s) => setIsStreaming(s)}
        onUsage={(u) => setUsage(u)}
        rightPanelOpen={pipelineOpen}
        showConfigTags={false}
        inputPlaceholder={t('agent.inputPlaceholder')}
        inputHintBar={
          <div
            style={{
              marginTop: 6,
              fontSize: 11,
              color: 'var(--text-muted)',
              display: 'flex',
              gap: 12,
            }}
          >
            <span>⏎ send</span>
            <span>⇧⏎ new line</span>
            <span>/ slash commands</span>
            <span>📎 drag to attach</span>
            {selectedProjectIds.length > 0 && (
              <span style={{ color: 'var(--accent-ai)' }}>
                📁 {selectedProjectIds.length} {t('agent.projectBound')}
              </span>
            )}
          </div>
        }
        onBeforeStream={() => {
          initPipeline();
          setActiveStage('discover');
          setStageStatus({ discover: 'running' });
          addLog('info', 'Agent development started');
          if (selectedProjectIds.length > 0) {
            addLog('info', `Binding ${selectedProjectIds.length} project(s) for context`);
          }
        }}
        onStreamFinished={(msg, completedToolCalls, resolvedThinking) => {
          const toolNames = completedToolCalls.map((tc) => tc.name.toLowerCase());
          if (
            toolNames.some(
              (n) =>
                n.includes('git') || n.includes('commit') || n.includes('push'),
            )
          ) {
            advanceStage('commit');
          }
          if (
            toolNames.some(
              (n) =>
                n.includes('test') ||
                n.includes('pytest') ||
                n.includes('cargo test'),
            )
          ) {
            advanceStage('test');
          }
          if (
            toolNames.some(
              (n) =>
                n.includes('build') ||
                n.includes('compile') ||
                n.includes('cargo build') ||
                n.includes('npm build'),
            )
          ) {
            advanceStage('build');
          }
          if (
            toolNames.some(
              (n) =>
                n.includes('write') ||
                n.includes('edit') ||
                n.includes('create') ||
                n.includes('str_replace'),
            )
          ) {
            advanceStage('write');
          }
          if (
            toolNames.some(
              (n) =>
                n.includes('read') || n.includes('glob') || n.includes('search'),
            )
          ) {
            advanceStage('analyze');
          }
          setStageStatus((prev) => ({
            ...prev,
            [activeStageRef.current]: 'success',
          }));
          addLog('success', 'Development turn completed');
        }}
        onAbortRef={(fn) => {
          abortRef.current = fn;
        }}
      />

      {/* Project selector drawer */}
      <ProjectSelector
        open={projectDrawerOpen}
        onClose={() => setProjectDrawerOpen(false)}
        projects={projects}
        selectedProjectIds={selectedProjectIds}
        onToggle={handleProjectToggle}
        onSelectAll={handleSelectAllProjects}
        onDeselectAll={handleDeselectAllProjects}
        loading={projectsLoading}
      />

      {/* Stage indicator (needs to be rendered above the chat) */}
      {/* NOTE: The StageIndicator is rendered inside ChatCore's top bar area via topBarExtra.
          PipelinePanel is rendered in the right column. */}
    </>
  );
}
