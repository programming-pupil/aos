import React from 'react';
import { Typography, Spin, Alert, Card, Badge, Tag } from 'antd';
import { useQuery } from '@tanstack/react-query';
import {
  BarChartOutlined,
  ClockCircleOutlined,
  ExperimentOutlined,
  RobotOutlined,
  SafetyCertificateOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons';
import i18n from 'i18next';
import { nl2sqlApi } from '@/api';

const { Text, Paragraph } = Typography;

interface ColumnNote {
  column: string;
  observation: string;
}

interface SqlExplanation {
  explanation: string;
  summary: string;
  insights?: string[];
  actions?: string[];
  risks?: string[];
  chart_recommendation?: string | null;
  column_notes?: ColumnNote[];
}

interface ExplainTabProps {
  sql: string | null;
  datasourceId: string | null;
  queryId: string | null;
  t: ReturnType<typeof import('react-i18next').useTranslation>['t'];
}

function isCacheExpiredError(error: Error): boolean {
  const msg = error.message ?? '';
  return msg.includes('410') || msg.includes('Gone') || msg.includes('expired') || msg.includes('cache');
}

function normalizeComparableText(text: string): string {
  return text
    .toLowerCase()
    .replace(/[\s,.;:!?，。；：！？、（）()[\]{}"'“”‘’`~\-_/\\|<>]+/g, '')
    .trim();
}

function isEffectivelyDuplicate(summary: string, explanation: string): boolean {
  const s = normalizeComparableText(summary);
  const e = normalizeComparableText(explanation);
  if (!s || !e) return false;
  if (s === e) return true;
  const shorter = s.length <= e.length ? s : e;
  const longer = s.length > e.length ? s : e;
  // Treat as duplicate when one text almost fully contains the other after normalization.
  return shorter.length >= 12 && longer.includes(shorter);
}

export function ExplainTab({ sql, datasourceId, queryId, t }: ExplainTabProps) {
  const normalizedSql = (sql ?? '').trim();
  const { data, isLoading, error, isError } = useQuery<SqlExplanation, Error>({
    queryKey: ['nl2sql', 'explain', queryId ?? '', datasourceId ?? '', normalizedSql],
    queryFn: () =>
      nl2sqlApi.explain({
        query_id: queryId!,
        data_source_id: datasourceId,
        sql: normalizedSql,
        language: i18n.language,
      }),
    enabled: !!queryId && normalizedSql.length > 0,
    staleTime: 30 * 1000,
    retry: 1,
  });

  if (normalizedSql.length === 0) {
    return (
      <div style={{ padding: 24, textAlign: 'center' }}>
        <Text style={{ color: 'var(--text-secondary)' }}>
          {t('explainTab.noSql')}
        </Text>
      </div>
    );
  }

  if (!queryId) {
    return (
      <div style={{ padding: 16 }}>
        <Alert
          type="info"
          message={t('explainTab.noSql')}
          showIcon
        />
      </div>
    );
  }

  if (isLoading) {
    return (
      <div style={{ padding: 32, textAlign: 'center' }}>
        <Spin />
      </div>
    );
  }

  if (isError && isCacheExpiredError(error!)) {
    return (
      <div style={{ padding: 16 }}>
        <Alert
          type="warning"
          icon={<ClockCircleOutlined />}
          message={t('explainTab.cacheExpiredTitle')}
          description={t('explainTab.cacheExpired')}
          showIcon
        />
      </div>
    );
  }

  if (isError || !data) {
    return (
      <div style={{ padding: 16 }}>
        <Alert
          type="error"
          message={t('explainTab.error')}
          description={error?.message}
          showIcon
        />
      </div>
    );
  }

  const insights = data.insights ?? [];
  const actions = data.actions ?? [];
  const risks = data.risks ?? [];
  const columnNotes = data.column_notes ?? [];
  const chartRecommendation = (data.chart_recommendation ?? '').trim();
  const showExplanation = !isEffectivelyDuplicate(data.summary ?? '', data.explanation ?? '');

  return (
    <div
      style={{
        padding: '12px 16px',
        display: 'flex',
        flexDirection: 'column',
        gap: 12,
        maxHeight: '100%',
        overflowY: 'auto',
      }}
    >
      {/* Summary — one-line overview with badge */}
      <Card
        size="small"
        style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
      >
        <div style={{ display: 'flex', alignItems: 'flex-start', gap: 10 }}>
          <ThunderboltOutlined
            style={{
              color: 'var(--accent-color)',
              marginTop: 4,
              fontSize: 16,
              flexShrink: 0,
            }}
          />
          <div style={{ flex: 1, minWidth: 0 }}>
            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                gap: 8,
                marginBottom: 6,
              }}
            >
              <Text
                style={{
                  fontSize: 11,
                  color: 'var(--text-secondary)',
                  textTransform: 'uppercase',
                  letterSpacing: '0.5px',
                  fontWeight: 600,
                }}
              >
                {t('explainTab.summary')}
              </Text>
            </div>
            <Badge
              color="var(--accent-color, #1677ff)"
              style={{ marginRight: 4 }}
            />
            <Text
              style={{
                fontSize: 13,
                color: 'var(--text-primary)',
                fontWeight: 500,
                lineHeight: 1.5,
              }}
            >
              {data.summary}
            </Text>
          </div>
        </div>
      </Card>

      {/* Detailed explanation card (hidden when it duplicates the summary) */}
      {showExplanation && (
        <Card
          size="small"
          title={
            <span
              style={{
                fontSize: 12,
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                color: 'var(--text-primary)',
              }}
            >
              <RobotOutlined style={{ color: 'var(--accent-color, #1677ff)' }} />
              {t('explainTab.explanation')}
            </span>
          }
          style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
          styles={{ body: { paddingTop: 12 } }}
        >
          <Paragraph
            style={{
              fontSize: 13,
              color: 'var(--text-primary)',
              whiteSpace: 'pre-wrap',
              lineHeight: 1.7,
              marginBottom: 0,
            }}
          >
            {data.explanation}
          </Paragraph>
        </Card>
      )}

      {/* Insights — only render when available */}
      {insights.length > 0 && (
        <Card
          size="small"
          title={
            <span
              style={{
                fontSize: 12,
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                color: 'var(--text-primary)',
              }}
            >
              <ExperimentOutlined style={{ color: 'var(--color-info, #58a6ff)' }} />
              {t('explainTab.insights')}
            </span>
          }
          style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
          styles={{ body: { padding: '12px 16px' } }}
        >
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {insights.map((insight, i) => (
              <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
                <Badge
                  count={i + 1}
                  style={{
                    backgroundColor: 'var(--accent-ai)',
                    marginTop: 2,
                    flexShrink: 0,
                  }}
                />
                <Text style={{ fontSize: 13, color: 'var(--text-primary)', lineHeight: 1.6 }}>
                  {insight}
                </Text>
              </div>
            ))}
          </div>
        </Card>
      )}

      {actions.length > 0 && (
        <Card
          size="small"
          title={
            <span
              style={{
                fontSize: 12,
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                color: 'var(--text-primary)',
              }}
            >
              <ThunderboltOutlined style={{ color: 'var(--color-success, #22c55e)' }} />
              {t('explainTab.actions')}
            </span>
          }
          style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
          styles={{ body: { padding: '12px 16px' } }}
        >
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {actions.map((action, i) => (
              <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
                <Badge
                  count={i + 1}
                  style={{ backgroundColor: 'var(--color-success, #22c55e)', marginTop: 2, flexShrink: 0 }}
                />
                <Text style={{ fontSize: 13, color: 'var(--text-primary)', lineHeight: 1.6 }}>
                  {action}
                </Text>
              </div>
            ))}
          </div>
        </Card>
      )}

      {(risks.length > 0 || chartRecommendation) && (
        <div style={{ display: 'grid', gridTemplateColumns: chartRecommendation && risks.length > 0 ? 'minmax(0, 1fr) minmax(0, 1fr)' : '1fr', gap: 12 }}>
          {risks.length > 0 && (
            <Card
              size="small"
              title={
                <span style={{ fontSize: 12, display: 'flex', alignItems: 'center', gap: 6, color: 'var(--text-primary)' }}>
                  <SafetyCertificateOutlined style={{ color: 'var(--color-warning, #f59e0b)' }} />
                  {t('explainTab.risks')}
                </span>
              }
              style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
            >
              <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                {risks.map((risk, i) => (
                  <Text key={i} style={{ fontSize: 12, color: 'var(--text-secondary)', lineHeight: 1.6 }}>
                    {risk}
                  </Text>
                ))}
              </div>
            </Card>
          )}
          {chartRecommendation && (
            <Card
              size="small"
              title={
                <span style={{ fontSize: 12, display: 'flex', alignItems: 'center', gap: 6, color: 'var(--text-primary)' }}>
                  <BarChartOutlined style={{ color: 'var(--accent-color, #1677ff)' }} />
                  {t('explainTab.chartRecommendation')}
                </span>
              }
              style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
            >
              <Text style={{ fontSize: 12, color: 'var(--text-secondary)', lineHeight: 1.6 }}>
                {chartRecommendation}
              </Text>
            </Card>
          )}
        </div>
      )}

      {/* Column notes — only render when available */}
      {columnNotes.length > 0 && (
        <Card
          size="small"
          title={
            <span
              style={{
                fontSize: 12,
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                color: 'var(--text-primary)',
              }}
            >
              {t('explainTab.columnNotes')}
            </span>
          }
          style={{ background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
          styles={{ body: { padding: '12px 16px' } }}
        >
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {columnNotes.map((note) => (
              <div
                key={note.column}
                style={{ display: 'flex', alignItems: 'baseline', gap: 8, flexWrap: 'wrap' }}
              >
                <Tag
                  style={{
                    fontFamily: 'var(--font-code)',
                    fontSize: 11,
                    background: 'var(--bg-hover)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-link)',
                    flexShrink: 0,
                  }}
                >
                  {note.column}
                </Tag>
                <Text style={{ fontSize: 12, color: 'var(--text-secondary)', lineHeight: 1.5 }}>
                  {note.observation}
                </Text>
              </div>
            ))}
          </div>
        </Card>
      )}
    </div>
  );
}
