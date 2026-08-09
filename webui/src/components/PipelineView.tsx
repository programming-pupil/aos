import { useState, useEffect, useRef } from 'react';
import { Typography, Button, Space, Tag, Progress } from 'antd';
import { useTranslation } from 'react-i18next';
import {
  CheckCircleFilled,
  CloseCircleFilled,
  LoadingOutlined,
  SyncOutlined,
  PauseCircleFilled,
  StopFilled,
  RocketOutlined,
  FileTextOutlined,
  BarChartOutlined,
  SearchOutlined,
} from '@ant-design/icons';
import type { MockPipelineStage, MockPipelineStep } from './mockData';

const { Text, Title } = Typography;

interface PipelineViewProps {
  stages: MockPipelineStage[];
  logs: Array<{ time: string; level: 'info' | 'success' | 'warn' | 'error'; msg: string }>;
  isRunning?: boolean;
  onPause?: () => void;
  onResume?: () => void;
  onAbort?: () => void;
  onRetry?: () => void;
  projectName?: string;
  taskName?: string;
  totalElapsed?: number;
  totalTokens?: { input: number; output: number; cost: number };
  /** Maps stage id -> display name. Useful for i18n. */
  stageNames?: Record<string, string>;
  /** Maps step id -> display name. Useful for i18n. */
  stepNames?: Record<string, string>;
}

type TabKey = 'logs' | 'diff' | 'usage' | 'review';

const STAGE_ORDER = ['clone', 'analyze', 'write', 'build', 'test', 'commit', 'deploy'];

// ── Stage Node ──────────────────────────────────────────────────────────────

function StageNode({
  stage,
  index,
  totalStages,
  onClick,
  isActive,
  labels,
}: {
  stage: MockPipelineStage;
  index: number;
  totalStages: number;
  onClick: () => void;
  isActive: boolean;
  labels: { stageNames?: Record<string, string>; pending: string; running: string; success: string; failed: string; skipped: string };
}) {
  const config = [
    { color: 'var(--pipeline-pending)', bg: 'var(--bg-interactive)', icon: '○', key: 'pending' },
    { color: 'var(--pipeline-running)', bg: 'rgba(124,58,237,0.15)', icon: '◎', key: 'running' },
    { color: 'var(--pipeline-success)', bg: 'rgba(63,185,80,0.12)', icon: '●', key: 'success' },
    { color: 'var(--pipeline-failed)', bg: 'rgba(248,81,73,0.12)', icon: '✕', key: 'failed' },
    { color: 'var(--pipeline-skipped)', bg: 'var(--bg-interactive)', icon: '⊘', key: 'skipped' },
  ];

  const cfg = config.find(c => c.key === stage.status) ?? config[0];
  const isLast = index === totalStages - 1;

  const CARD_MIN_WIDTH = 140;

  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: '2vmin', overflowX: 'auto', flexShrink: 0 }}>
      {/* Stage card */}
      <div
        onClick={onClick}
        style={{
          minWidth: CARD_MIN_WIDTH,
          flexShrink: 0,
          padding: '14px 14px',
          background: isActive ? cfg.bg : 'var(--bg-elevated)',
          border: `1.5px solid ${isActive ? cfg.color : 'var(--border-default)'}`,
          borderRadius: 10,
          cursor: 'pointer',
          transition: 'all var(--transition-base)',
          textAlign: 'center',
          boxShadow: isActive ? `0 0 16px ${cfg.color}40` : 'none',
          position: 'relative',
        }}
      >
        {/* Icon */}
        <div style={{ fontSize: 22, marginBottom: 4 }}>
          {stage.icon}
        </div>
        {/* Stage name */}
        <Text style={{ fontSize: 13, color: 'var(--text-secondary)', display: 'block', fontWeight: 600 }} ellipsis>
          {labels?.stageNames?.[stage.id] ?? stage.name}
        </Text>
        {/* Status */}
        <div style={{ marginTop: 4, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 4 }}>
          {stage.status === 'success' && (
            <CheckCircleFilled style={{ fontSize: 12, color: cfg.color }} />
          )}
          {stage.status === 'failed' && (
            <CloseCircleFilled style={{ fontSize: 12, color: cfg.color }} />
          )}
          {stage.status === 'running' && (
            <LoadingOutlined style={{ fontSize: 12, color: cfg.color, animation: 'spin 1s linear infinite' }} />
          )}
          {stage.status === 'pending' && (
            <span style={{ fontSize: 10, color: cfg.color }}>○</span>
          )}
          <Text style={{ fontSize: 10, color: cfg.color }}>{labels[stage.status as 'pending' | 'running' | 'success' | 'failed' | 'skipped'] ?? labels.pending}</Text>
        </div>
        {/* Duration */}
        {stage.duration && (
          <Text style={{ fontSize: 10, color: 'var(--text-muted)', display: 'block', marginTop: 2 }}>
            {stage.duration}s
          </Text>
        )}
        {/* Running pulse */}
        {stage.status === 'running' && (
          <div style={{
            position: 'absolute',
            top: '50%',
            left: '50%',
            transform: 'translate(-50%,-50%)',
            width: '100%',
            height: '100%',
            borderRadius: 10,
            border: `2px solid ${cfg.color}`,
            animation: 'pulse 2s ease-in-out infinite',
            pointerEvents: 'none',
          }} />
        )}
      </div>

      {/* Connector */}
      {!isLast && (
        <div style={{
          width: 0,
          height: 2,
          background: stage.status === 'success' ? 'var(--pipeline-success)' : 'var(--border-default)',
          position: 'relative',
          flexShrink: 0,
        }}>
          {stage.status === 'success' && (
            <div style={{
              position: 'absolute',
              top: -1,
              left: 0,
              width: 8,
              height: 4,
              background: 'var(--pipeline-success)',
              borderRadius: 2,
              animation: 'pipelineFlow 1s ease-in-out forwards',
            }} />
          )}
        </div>
      )}
    </div>
  );
}

// ── Log Line ────────────────────────────────────────────────────────────────

function LogLine({ entry }: { entry: { time: string; level: string; msg: string } }) {
  const colors: Record<string, string> = {
    info: 'var(--text-secondary)',
    success: 'var(--color-success)',
    warn: 'var(--color-warning)',
    error: 'var(--color-error)',
  };
  const icons: Record<string, string> = {
    info: '▶',
    success: '✓',
    warn: '⚠',
    error: '✗',
  };
  return (
    <div style={{
      display: 'flex',
      alignItems: 'flex-start',
      gap: 8,
      padding: '2px 0',
      fontFamily: 'var(--font-code)',
      fontSize: 12,
      color: colors[entry.level] ?? colors.info,
      animation: 'fadeSlideInLeft 100ms ease forwards',
    }}>
      <span style={{ color: 'var(--text-muted)', flexShrink: 0, fontSize: 10 }}>{entry.time}</span>
      <span style={{ flexShrink: 0 }}>{icons[entry.level] ?? icons.info}</span>
      <span>{entry.msg}</span>
    </div>
  );
}

// ── Step List ──────────────────────────────────────────────────────────────

function StepList({ steps, nameMap }: { steps: MockPipelineStep[]; nameMap?: Record<string, string> }) {
  return (
    <div>
      {steps.map((step) => {
        const colors: Record<string, string> = {
          success: 'var(--color-success)',
          running: 'var(--accent-ai)',
          failed: 'var(--color-error)',
          pending: 'var(--text-muted)',
        };
        const icons: Record<string, React.ReactNode> = {
          success: <CheckCircleFilled style={{ fontSize: 11 }} />,
          running: <LoadingOutlined style={{ fontSize: 11, animation: 'spin 1s linear infinite' }} />,
          failed: <CloseCircleFilled style={{ fontSize: 11 }} />,
          pending: <span style={{ fontSize: 10 }}>○</span>,
        };
        return (
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
            <span style={{ color: colors[step.status], width: 14 }}>{icons[step.status]}</span>
            <Text style={{ fontSize: 12, color: 'var(--text-secondary)', fontFamily: 'var(--font-code)', flex: 1 }}>
              {nameMap?.[step.id] ?? step.name}
            </Text>
            {step.duration !== undefined && (
              <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>{step.duration}s</Text>
            )}
            {step.status === 'running' && (
              <Progress
                size="small"
                percent={50}
                showInfo={false}
                strokeColor="var(--accent-ai)"
                style={{ width: 60 }}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

// ── Pipeline View ───────────────────────────────────────────────────────────

export function PipelineView({
  stages,
  logs,
  isRunning = true,
  onPause,
  onResume,
  onAbort,
  onRetry,
  projectName = 'user-service',
  taskName = 'OAuth2 Authentication Module Integration',
  totalElapsed = 154,
  totalTokens,
  stageNames,
  stepNames,
}: PipelineViewProps) {
  const { t } = useTranslation();
  const [activeTab, setActiveTab] = useState<TabKey>('logs');
  const [selectedStageId, setSelectedStageId] = useState<string | null>(null);
  const [elapsed, setElapsed] = useState(totalElapsed);
  const logsEndRef = useRef<HTMLDivElement>(null);

  // Simulate elapsed time
  useEffect(() => {
    if (!isRunning) return;
    const timer = setInterval(() => setElapsed((e) => e + 1), 1000);
    return () => clearInterval(timer);
  }, [isRunning]);

  // Auto-scroll logs
  useEffect(() => {
    logsEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [logs.length]);

  const selectedStage = stages.find((s) => s.id === selectedStageId) ?? stages.find((s) => s.status === 'running') ?? stages[0];

  // Pipeline status
  const hasFailed = stages.some((s) => s.status === 'failed');
  const allDone = stages.every((s) => s.status === 'success' || s.status === 'skipped' || s.status === 'pending');
  const overallStatus = hasFailed ? 'failed' : allDone ? 'success' : 'running';

  const formatElapsed = (s: number) => {
    const m = Math.floor(s / 60);
    const sec = s % 60;
    return m > 0 ? `${m}m ${sec}s` : `${sec}s`;
  };

  // Failed stage
  const failedStage = stages.find((s) => s.status === 'failed');

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      {/* ── Header ───────────────────────────────────────────────────────── */}
      <div
        style={{
          padding: '12px 20px',
          background: 'var(--bg-surface)',
          borderBottom: '1px solid var(--border-subtle)',
          display: 'flex',
          alignItems: 'center',
          gap: 12,
          flexShrink: 0,
        }}
      >
        <span style={{ fontSize: 20 }}>🚀</span>
        <div style={{ flex: 1 }}>
          <Title level={5} style={{ margin: 0, fontSize: 15, color: 'var(--text-primary)' }}>
            {t('pipeline.pipelineTitle')}
          </Title>
          <Text style={{ fontSize: 12, color: 'var(--text-muted)' }}>
            {taskName} — {projectName}
          </Text>
        </div>

        {/* Status */}
        <div style={{
          padding: '4px 12px',
          borderRadius: 20,
          background: overallStatus === 'success' ? 'var(--color-success-bg)' :
            overallStatus === 'failed' ? 'var(--color-error-bg)' :
            'var(--accent-ai-muted)',
          fontSize: 12,
          fontWeight: 600,
          color: overallStatus === 'success' ? 'var(--color-success)' :
            overallStatus === 'failed' ? 'var(--color-error)' :
            'var(--accent-ai)',
          display: 'flex',
          alignItems: 'center',
          gap: 6,
        }}>
          {overallStatus === 'success' && <CheckCircleFilled style={{ fontSize: 12 }} />}
          {overallStatus === 'failed' && <CloseCircleFilled style={{ fontSize: 12 }} />}
          {overallStatus === 'running' && <LoadingOutlined style={{ fontSize: 12 }} />}
          {overallStatus === 'success' ? t('pipeline.completed') : overallStatus === 'failed' ? t('pipeline.failed') : t('pipeline.running')}
        </div>

        <Text style={{ fontSize: 13, color: 'var(--text-muted)', minWidth: 60, textAlign: 'right' }}>
          {t('pipeline.elapsed', { time: formatElapsed(elapsed) })}
        </Text>

        {/* Controls */}
        <Space size={4}>
          {overallStatus === 'running' && onPause && (
            <Button size="small" icon={<PauseCircleFilled />} onClick={onPause} style={{ color: 'var(--text-secondary)', borderColor: 'var(--border-default)' }}>
              {t('pipeline.pause')}
            </Button>
          )}
          {overallStatus === 'failed' && onRetry && (
            <Button size="small" icon={<SyncOutlined spin />} onClick={onRetry} style={{ color: 'var(--text-secondary)', borderColor: 'var(--border-default)' }}>
              {t('pipeline.retry')}
            </Button>
          )}
          {onAbort && (
            <Button size="small" danger icon={<StopFilled />} onClick={onAbort} style={{}}>
              {t('pipeline.abort')}
            </Button>
          )}
        </Space>
      </div>

      {/* ── Pipeline Overview ────────────────────────────────────────────── */}
      <div
        style={{
          padding: '16px 20px',
          background: 'var(--bg-elevated)',
          borderBottom: '1px solid var(--border-subtle)',
          flexShrink: 0,
          overflow: 'hidden',
        }}
      >
        <div style={{
          display: 'grid',
          gridAutoFlow: 'column',
          gridTemplateColumns: `repeat(${stages.length}, minmax(120px, 1fr))`,
          gap: '1vmin',
          alignItems: 'center',
          minWidth: 0,
        }}>
          {stages.map((stage, index) => (
            <StageNode
              key={stage.id}
              stage={stage}
              index={index}
              totalStages={stages.length}
              onClick={() => setSelectedStageId(stage.id)}
              isActive={stage.id === selectedStage?.id}
              labels={{ stageNames, pending: t('agent.idle'), running: t('agent.running'), success: t('agent.completed'), failed: t('agent.failed'), skipped: 'Skipped' }}
            />
          ))}
        </div>
      </div>

      {/* ── Bottom Panel ────────────────────────────────────────────────── */}
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
        {/* Tabs */}
        <div
          style={{
            display: 'flex',
            gap: 0,
            padding: '0 20px',
            borderBottom: '1px solid var(--border-subtle)',
            background: 'var(--bg-surface)',
            flexShrink: 0,
          }}
        >
          {([
            { key: 'logs', icon: '📋', label: t('pipeline.tabs.logs') },
            { key: 'diff', icon: '📝', label: t('pipeline.tabs.diff') },
            { key: 'usage', icon: '📊', label: t('pipeline.tabs.usage') },
            { key: 'review', icon: '🔍', label: t('pipeline.tabs.review') },
          ] as const).map((tab) => (
            <div
              key={tab.key}
              onClick={() => setActiveTab(tab.key as TabKey)}
              style={{
                padding: '10px 16px',
                cursor: 'pointer',
                borderBottom: `2px solid ${activeTab === tab.key ? 'var(--accent-ai)' : 'transparent'}`,
                color: activeTab === tab.key ? 'var(--text-primary)' : 'var(--text-muted)',
                fontSize: 13,
                fontWeight: activeTab === tab.key ? 600 : 400,
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                transition: 'all var(--transition-fast)',
                marginBottom: -1,
              }}
            >
              <span>{tab.icon}</span>
              {tab.label}
            </div>
          ))}
        </div>

        {/* Tab content */}
        <div style={{ flex: 1, overflow: 'hidden', display: 'flex' }}>
          {/* Left: stage details */}
          <div style={{ width: 280, borderRight: '1px solid var(--border-subtle)', padding: '12px 16px', overflow: 'auto', flexShrink: 0 }}>
          <Text style={{ fontSize: 12, color: 'var(--text-muted)', textTransform: 'uppercase', letterSpacing: '0.08em', fontWeight: 600 }}>
              {stageNames?.[selectedStage?.id ?? ''] ?? selectedStage?.name} — {t('pipeline.stageSteps')}
          </Text>
            <div style={{ marginTop: 8 }}>
              <StepList steps={selectedStage?.steps ?? []} nameMap={stepNames} />
            </div>
          </div>

          {/* Right: tab content */}
          <div style={{ flex: 1, overflow: 'auto', padding: '12px 16px' }}>
            {activeTab === 'logs' && (
              <div>
                <div style={{
                  fontFamily: 'var(--font-code)',
                  fontSize: 12,
                  background: 'var(--bg-void)',
                  borderRadius: 8,
                  padding: '8px 12px',
                  maxHeight: 400,
                  overflow: 'auto',
                }}>
                  {logs.map((log, i) => (
                    <LogLine key={i} entry={log} />
                  ))}
                  {isRunning && (
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8, color: 'var(--accent-ai)', fontSize: 12 }}>
                      <LoadingOutlined style={{ animation: 'spin 1s linear infinite' }} />
                      <span>{t('pipeline.agentWorking')}</span>
                    </div>
                  )}
                  <div ref={logsEndRef} />
                </div>
              </div>
            )}

            {activeTab === 'diff' && (
              <div>
                <div style={{
                  background: 'var(--bg-void)',
                  borderRadius: 8,
                  padding: '8px 0',
                  fontFamily: 'var(--font-code)',
                  fontSize: 12,
                  maxHeight: 400,
                  overflow: 'auto',
                }}>
                  {[
                    { file: 'src/middleware/auth.ts', lines: [
                      { n: 74, type: 'add', code: '  // OAuth2 token validation' },
                      { n: 75, type: 'add', code: '  if (token.startsWith(\'oauth2_\')) {' },
                      { n: 76, type: 'add', code: '    return await validateOAuth2Token(token);' },
                      { n: 77, type: 'add', code: '  }' },
                      { n: 78, type: 'keep', code: '  return jwt.verify(token, secret);' },
                    ]},
                    { file: 'src/services/oauth2.ts', lines: [
                      { n: 1, type: 'add', code: 'import { OAuth2Provider } from \'../types/oauth2\';' },
                      { n: 2, type: 'add', code: '' },
                      { n: 3, type: 'add', code: 'export class OAuth2Service {' },
                      { n: 4, type: 'add', code: '  async authenticate(provider: OAuth2Provider, code: string) {' },
                      { n: 5, type: 'add', code: '    // Implementation...' },
                    ]},
                  ].map(({ file, lines }) => (
                    <div key={file}>
                      <div style={{ padding: '4px 12px', background: 'var(--bg-surface)', color: 'var(--text-muted)', fontSize: 11, borderBottom: '1px solid var(--border-subtle)' }}>
                        📁 {file}
                      </div>
                      {lines.map((line) => (
                        <div
                          key={line.n}
                          style={{
                            display: 'flex',
                            padding: '1px 12px',
                            background: line.type === 'add' ? 'rgba(63,185,80,0.08)' : line.type === 'remove' ? 'rgba(248,81,73,0.08)' : 'transparent',
                            borderLeft: `3px solid ${line.type === 'add' ? 'var(--color-success)' : line.type === 'remove' ? 'var(--color-error)' : 'transparent'}`,
                          }}
                        >
                          <span style={{ color: 'var(--text-muted)', width: 32, flexShrink: 0, fontSize: 11 }}>{line.n}</span>
                          <span style={{
                            color: line.type === 'add' ? 'var(--color-success)' : line.type === 'remove' ? 'var(--color-error)' : 'var(--text-secondary)',
                            flex: 1,
                          }}>{line.code}</span>
                        </div>
                      ))}
                    </div>
                  ))}
                </div>
              </div>
            )}

            {activeTab === 'usage' && (
              <div>
                <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 12 }}>
                  {[
                    { label: t('pipeline.usage.inputTokens'), value: totalTokens?.input.toLocaleString() ?? '12,400', color: 'var(--color-info)' },
                    { label: t('pipeline.usage.outputTokens'), value: totalTokens?.output.toLocaleString() ?? '8,200', color: 'var(--color-success)' },
                    { label: t('pipeline.usage.cost'), value: `$${totalTokens?.cost ?? 0.04}`, color: 'var(--accent-ai)' },
                    { label: t('pipeline.usage.elapsed'), value: formatElapsed(elapsed), color: 'var(--text-primary)' },
                    { label: t('pipeline.usage.changedFiles'), value: '5', color: 'var(--text-secondary)' },
                    { label: t('pipeline.usage.testsPassed'), value: '42 / 42', color: 'var(--color-success)' },
                  ].map((stat) => (
                    <div key={stat.label} style={{
                      padding: '12px 14px',
                      background: 'var(--bg-elevated)',
                      borderRadius: 8,
                      border: '1px solid var(--border-subtle)',
                      textAlign: 'center',
                    }}>
                      <div style={{ fontSize: 20, fontWeight: 700, color: stat.color }}>{stat.value}</div>
                      <div style={{ fontSize: 11, color: 'var(--text-muted)', marginTop: 2 }}>{stat.label}</div>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {activeTab === 'review' && (
              <div style={{ textAlign: 'center', padding: '40px 0' }}>
                <span style={{ fontSize: 40 }}>🔍</span>
                <div style={{ marginTop: 12, fontSize: 14, color: 'var(--text-secondary)' }}>
                  {t('pipeline.review.unavailable')}
                </div>
                <div style={{ marginTop: 4, fontSize: 12, color: 'var(--text-muted)' }}>
                  {t('pipeline.review.autoGenerate')}
                </div>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* ── Error Alert ────────────────────────────────────────────────── */}
      {failedStage && (
        <div
          style={{
            margin: '0 20px 12px',
            padding: '12px 14px',
            background: 'var(--color-error-bg)',
            border: '1px solid rgba(248,81,73,0.3)',
            borderRadius: 8,
            animation: 'shake 0.3s ease',
          }}
        >
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 6 }}>
            <span style={{ fontSize: 16 }}>⛔</span>
            <Text style={{ fontSize: 13, fontWeight: 600, color: 'var(--color-error)' }}>
              {failedStage.name} {t('agent.failed')}
            </Text>
          </div>
          <div style={{
            fontFamily: 'var(--font-code)',
            fontSize: 11,
            color: 'var(--text-secondary)',
            background: 'var(--bg-surface)',
            borderRadius: 4,
            padding: '6px 10px',
          }}>
            error[E0603]: struct `User` has no field `provider_id`<br />
            &nbsp;&nbsp;--&gt; src/models/user.rs:76:28<br />
            &nbsp;&nbsp;&nbsp;&nbsp;self.provider_id.clone()<br />
            &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;^^^^^^^^^^^^^
          </div>
        </div>
      )}

      {/* ── Summary Footer ──────────────────────────────────────────────── */}
      <div
        style={{
          padding: '8px 20px',
          background: 'var(--bg-surface)',
          borderTop: '1px solid var(--border-subtle)',
          display: 'flex',
          alignItems: 'center',
          gap: 16,
          flexShrink: 0,
        }}
      >
        <Text style={{ fontSize: 12, color: 'var(--text-muted)' }}>
          {t('pipeline.usage.elapsed')}: <span style={{ color: 'var(--text-primary)' }}>{formatElapsed(elapsed)}</span>
        </Text>
        <Text style={{ fontSize: 12, color: 'var(--text-muted)' }}>
          {t('agent.tokens.input')}: <span style={{ color: 'var(--color-info)' }}>⬆ {(totalTokens?.input ?? 12400).toLocaleString()}</span>
          <span style={{ color: 'var(--text-muted)' }}> / </span>
          <span style={{ color: 'var(--color-success)' }}>⬇ {(totalTokens?.output ?? 8200).toLocaleString()}</span>
        </Text>
        <Text style={{ fontSize: 12, color: 'var(--text-muted)' }}>
          {t('pipeline.usage.cost')}: <span style={{ color: 'var(--accent-ai)' }}>${(totalTokens?.cost ?? 0.04).toFixed(2)}</span>
        </Text>
        <Text style={{ fontSize: 12, color: 'var(--text-muted)', marginLeft: 'auto' }}>
          {t('pipeline.progress', { current: stages.filter(s => s.status === 'success').length, total: stages.length, context: 'progress' })}
        </Text>
      </div>
    </div>
  );
}
