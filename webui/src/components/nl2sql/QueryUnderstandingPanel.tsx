import React, { useState } from 'react';
import { Card, Tag, Tooltip, Typography } from 'antd';
import {
  QuestionCircleOutlined, ClockCircleOutlined, FilterOutlined,
  FunctionOutlined, SwapOutlined, TableOutlined,
  UpOutlined, DownOutlined, CheckCircleOutlined,
} from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { QueryUnderstandingResponse } from '@/types';

const { Text, Paragraph } = Typography;

interface QueryUnderstandingPanelProps {
  /** Parsed query understanding result from the backend. */
  data: QueryUnderstandingResponse;
  /** The original user question for comparison with rewritten version. */
  originalQuestion: string;
}

type EntityChipColor = 'blue' | 'green' | 'orange' | 'purple' | 'cyan' | 'red' | 'gold';

function intentBadge(intent: string): { label: string; color: EntityChipColor } {
  const lower = intent.toLowerCase();
  if (lower.includes('select') || lower.includes('query') || lower.includes('retrieve') || lower.includes('get')) {
    return { label: 'SELECT', color: 'blue' };
  }
  if (lower.includes('count') || lower.includes('sum') || lower.includes('aggregate')) {
    return { label: 'AGGREGATE', color: 'green' };
  }
  if (lower.includes('compare') || lower.includes('versus')) {
    return { label: 'COMPARE', color: 'orange' };
  }
  if (lower.includes('trend') || lower.includes('over time') || lower.includes('timeseries')) {
    return { label: 'TREND', color: 'purple' };
  }
  if (lower.includes('rank') || lower.includes('top') || lower.includes('bottom')) {
    return { label: 'RANK', color: 'cyan' };
  }
  return { label: intent.toUpperCase(), color: 'blue' };
}

function ConfidenceBar({ confidence }: { confidence: number }) {
  const pct = Math.round(confidence * 100);
  const color = pct >= 80 ? '#52c41a' : pct >= 60 ? '#faad14' : '#ff4d4f';
  return (
    <Tooltip title={`${pct}%`}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
        <div style={{
          flex: 1,
          height: 4,
          background: 'var(--border-subtle)',
          borderRadius: 2,
          overflow: 'hidden',
        }}>
          <div style={{
            width: `${pct}%`,
            height: '100%',
            background: color,
            borderRadius: 2,
            transition: 'width 0.3s',
          }} />
        </div>
        <Text style={{ fontSize: 11, color: 'var(--text-muted)', minWidth: 32 }}>{pct}%</Text>
      </div>
    </Tooltip>
  );
}

function SectionHeader({
  icon,
  label,
  count,
  accent = 'var(--accent-ai)',
  empty,
}: {
  icon: React.ReactNode;
  label: string;
  count?: number;
  accent?: string;
  empty?: boolean;
}) {
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 6,
      marginBottom: count && count > 0 ? 4 : 0,
    }}>
      <span style={{ color: accent, fontSize: 12 }}>{icon}</span>
      <Text style={{ fontSize: 11, fontWeight: 500, color: 'var(--text-secondary)' }}>{label}</Text>
      {count !== undefined && count > 0 && (
        <Tag
          style={{
            fontSize: 9,
            padding: '0 5px',
            lineHeight: '14px',
            marginInlineEnd: 0,
            borderRadius: 999,
            borderColor: 'rgba(148,163,184,0.35)',
            color: 'var(--text-muted)',
            background: 'rgba(15,23,42,0.45)',
          }}
        >
          {count}
        </Tag>
      )}
      {empty && (
        <Text style={{ fontSize: 11, color: 'var(--text-muted)', fontStyle: 'italic' }}>{count === 0 ? '' : ''}</Text>
      )}
    </div>
  );
}

function FilterItem({ f }: { f: { column: string; value: string; op: string; raw: string } }) {
  return (
    <div style={{
      display: 'inline-flex', alignItems: 'center', gap: 4,
      background: 'rgba(59, 130, 246, 0.12)',
      border: '1px solid rgba(59, 130, 246, 0.32)',
      borderRadius: 6,
      padding: '2px 7px',
      fontSize: 10,
    }}>
      <Text style={{ color: '#93c5fd', fontWeight: 400, fontSize: 10 }}>{f.column}</Text>
      <Text style={{ color: 'var(--text-muted)', fontSize: 10 }}>{f.op}</Text>
      <Text style={{ color: '#bfdbfe', fontSize: 10 }}>{f.value}</Text>
    </div>
  );
}

function AggregationChip({ agg }: { agg: string }) {
  return (
    <Tag
      style={{
        fontSize: 10,
        marginInlineEnd: 0,
        color: '#c4b5fd',
        borderColor: 'rgba(167,139,250,0.35)',
        background: 'rgba(124,58,237,0.14)',
      }}
    >
      {agg}
    </Tag>
  );
}

function ComparisonChip({ comp }: { comp: { type: string; raw: string } }) {
  return (
    <Tooltip title={comp.raw}>
      <Tag
      style={{
          fontSize: 10,
          marginInlineEnd: 0,
          color: '#fdba74',
          borderColor: 'rgba(251,146,60,0.35)',
          background: 'rgba(249,115,22,0.14)',
        }}
      >
        {comp.type || 'comparison'}
      </Tag>
    </Tooltip>
  );
}

export function QueryUnderstandingPanel({ data, originalQuestion }: QueryUnderstandingPanelProps) {
  const { t } = useTranslation();
  const [collapsed, setCollapsed] = useState(true);

  const { rewrittenQuestion, intent, entities, confidence } = data;
  const intentInfo = intentBadge(intent);

  const hasTime = !!entities?.time;
  const hasSubject = !!(entities?.subject && (entities.subject.tables?.length || entities.subject.columns?.length));
  const hasFilters = !!(entities?.filters && entities.filters.length > 0);
  const hasAggregations = !!(entities?.aggregations && entities.aggregations.length > 0);
  const hasComparisons = !!(entities?.comparisons && entities.comparisons.length > 0);
  const hasRewritten = rewrittenQuestion && rewrittenQuestion !== originalQuestion;

  const hasAnyEntity = hasTime || hasSubject || hasFilters || hasAggregations || hasComparisons;

  if (!hasAnyEntity) {
    return null;
  }

  return (
    <div style={{ marginBottom: 8 }}>
      <Card
        size="small"
        style={{
          background: 'rgba(99, 102, 241, 0.03)',
          border: '1px solid rgba(99, 102, 241, 0.15)',
          borderRadius: 10,
        }}
        bodyStyle={{ padding: '8px 10px' }}
      >
        {/* Header row */}
        <div style={{
          display: 'flex', alignItems: 'center', gap: 8,
          marginBottom: !collapsed && hasAnyEntity ? 6 : 0,
          cursor: 'pointer',
        }}
          onClick={() => setCollapsed(c => !c)}
        >
          <QuestionCircleOutlined style={{ color: '#7c3aed', fontSize: 12 }} />
          <Text style={{ fontSize: 11, fontWeight: 500, color: 'var(--text-secondary)', flex: 1 }}>
            {t('nl2sql.queryUnderstanding.title')}
          </Text>
          <Tag
            color={intentInfo.color}
            style={{ fontSize: 9, padding: '0 4px', lineHeight: '14px', marginRight: 2 }}
          >
            {intentInfo.label}
          </Tag>
          <ConfidenceBar confidence={confidence} />
          <button
            style={{
              background: 'none', border: 'none', cursor: 'pointer',
              padding: '2px 4px', display: 'flex', alignItems: 'center',
              color: 'var(--text-muted)',
            }}
            onClick={(e) => { e.stopPropagation(); setCollapsed(c => !c); }}
          >
            {collapsed ? <DownOutlined style={{ fontSize: 11 }} /> : <UpOutlined style={{ fontSize: 11 }} />}
          </button>
        </div>

        {/* Rewritten question */}
        {hasRewritten && !collapsed && (
          <div style={{
            background: 'rgba(82, 196, 26, 0.06)',
            border: '1px solid rgba(82, 196, 26, 0.2)',
            borderRadius: 6,
            padding: '4px 8px',
            marginBottom: 6,
          }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 4, marginBottom: 2 }}>
              <CheckCircleOutlined style={{ color: '#52c41a', fontSize: 10 }} />
              <Text style={{ fontSize: 10, color: '#52c41a', fontWeight: 500 }}>{t('nl2sql.queryUnderstanding.rewritten')}</Text>
            </div>
            <Paragraph
              style={{ margin: 0, fontSize: 11, color: 'var(--text-primary)', lineHeight: 1.35 }}
              ellipsis={{ rows: 1, expandable: false }}
            >
              {rewrittenQuestion}
            </Paragraph>
          </div>
        )}

        {!collapsed && (
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: 'repeat(auto-fit, minmax(170px, 1fr))',
              gap: 6,
              alignItems: 'start',
            }}
          >
            {/* Time range */}
            {hasTime && entities.time && (
              <div style={{
                background: 'rgba(250, 173, 20, 0.06)',
                border: '1px solid rgba(250, 173, 20, 0.2)',
                borderRadius: 6,
                padding: '5px 7px',
              }}>
                <SectionHeader
                  icon={<ClockCircleOutlined />}
                  label={t('nl2sql.queryUnderstanding.timeRange')}
                  count={1}
                />
                <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4 }}>
                  <Tag color="gold" style={{ fontSize: 9, marginInlineEnd: 0 }}>
                    {entities.time.resolvedType || entities.time.raw}
                  </Tag>
                  {entities.time.granularity && (
                    <Tag style={{ fontSize: 9, background: 'rgba(250,173,20,0.1)', border: 'none', marginInlineEnd: 0 }}>
                      {entities.time.granularity}
                    </Tag>
                  )}
                  {entities.time.ranges?.map(([start, end], i) => (
                    <Text key={i} style={{ fontSize: 9, color: 'var(--text-secondary)' }}>
                      {start} — {end}
                    </Text>
                  ))}
                </div>
              </div>
            )}

            {/* Subject */}
            {hasSubject && entities.subject && (
              <div style={{
                background: 'rgba(99, 102, 241, 0.07)',
                border: '1px solid rgba(99, 102, 241, 0.24)',
                borderRadius: 8,
                padding: '5px 7px',
              }}>
                <SectionHeader
                  icon={<TableOutlined />}
                  label={t('nl2sql.queryUnderstanding.subject')}
                  count={(entities.subject.tables?.length || 0) + (entities.subject.columns?.length || 0)}
                  accent="#818cf8"
                />
                <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4 }}>
                  {entities.subject.tables?.map((table, i) => (
                    <Tag
                      key={`t-${i}`}
                      style={{
                        fontSize: 9,
                        marginInlineEnd: 0,
                        color: '#e2e8f0',
                        borderColor: 'rgba(148,163,184,0.35)',
                        background: 'rgba(30,41,59,0.55)',
                      }}
                    >
                      {table}
                    </Tag>
                  ))}
                  {entities.subject.columns?.map((col, i) => (
                    <Tag
                      key={`c-${i}`}
                      style={{
                        fontSize: 9,
                        marginInlineEnd: 0,
                        color: '#cbd5e1',
                        borderColor: 'rgba(148,163,184,0.28)',
                        background: 'rgba(15,23,42,0.45)',
                      }}
                    >
                      {col}
                    </Tag>
                  ))}
                </div>
              </div>
            )}

            {/* Filters */}
            {hasFilters && entities.filters && (
              <div style={{
                background: 'rgba(56, 189, 248, 0.08)',
                border: '1px solid rgba(56, 189, 248, 0.26)',
                borderRadius: 8,
                padding: '5px 7px',
              }}>
                <SectionHeader
                  icon={<FilterOutlined />}
                  label={t('nl2sql.queryUnderstanding.filters')}
                  count={entities.filters.length}
                  accent="#38bdf8"
                />
                <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4 }}>
                  {entities.filters.map((f, i) => (
                    <FilterItem key={i} f={f} />
                  ))}
                </div>
              </div>
            )}

            {/* Aggregations */}
            {hasAggregations && entities.aggregations && (
              <div style={{
                background: 'rgba(167, 139, 250, 0.08)',
                border: '1px solid rgba(167, 139, 250, 0.26)',
                borderRadius: 8,
                padding: '5px 7px',
              }}>
                <SectionHeader
                  icon={<FunctionOutlined />}
                  label={t('nl2sql.queryUnderstanding.aggregations')}
                  count={entities.aggregations.length}
                  accent="#a78bfa"
                />
                <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4 }}>
                  {entities.aggregations.map((agg, i) => (
                    <AggregationChip key={i} agg={agg} />
                  ))}
                </div>
              </div>
            )}

            {/* Comparisons */}
            {hasComparisons && entities.comparisons && (
              <div style={{
                background: 'rgba(251, 146, 60, 0.08)',
                border: '1px solid rgba(251, 146, 60, 0.24)',
                borderRadius: 8,
                padding: '5px 7px',
              }}>
                <SectionHeader
                  icon={<SwapOutlined />}
                  label={t('nl2sql.queryUnderstanding.comparisons')}
                  count={entities.comparisons.length}
                  accent="#fb923c"
                />
                <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4 }}>
                  {entities.comparisons.map((comp, i) => (
                    <ComparisonChip key={i} comp={comp} />
                  ))}
                </div>
              </div>
            )}
          </div>
        )}
      </Card>
    </div>
  );
}
