import React, { useEffect, useRef, useState } from 'react';
import { Card, Button, Radio, Input, Space, Typography, Tag } from 'antd';
import { useTranslation } from 'react-i18next';
import { QuestionCircleOutlined } from '@ant-design/icons';
import type { ClarificationContext as ClarificationContextAlias, ClarifyOption } from '@/types';

const { Text } = Typography;

// Re-export so consumers can import both from one place.
export type { ClarificationContextAlias as ClarificationContext };

interface ClarificationCardProps {
  context: ClarificationContextAlias;
  onSelect: (option: ClarifyOption) => void;
  onFreeText: (text: string) => void;
  onCancel?: () => void;
  loading?: boolean;
  progressStage?: string | null;
  t: ReturnType<typeof useTranslation>['t'];
}

export function ClarificationCard({
  context,
  onSelect,
  onFreeText,
  onCancel,
  loading,
  progressStage,
  t,
}: ClarificationCardProps) {
  const [freeText, setFreeText] = useState('');
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  const composingRef = useRef(false);
  const lastCompositionEndAtRef = useRef(0);
  const stageOrder = ['queued', 'request_validation', 'load_schema', 'load_context', 'query_understanding', 'clarification_gate', 'cache_lookup', 'generate_sql', 'done'] as const;
  const stageLabelMap: Record<string, string> = {
    queued: t('nl2sql.clarification.progressStageSubmit'),
    request_validation: t('nl2sql.clarification.progressStageSubmit'),
    load_schema: t('nl2sql.clarification.progressStageSchema'),
    load_context: t('nl2sql.clarification.progressStageContext'),
    query_understanding: t('nl2sql.clarification.progressStageIntent'),
    clarification_gate: t('nl2sql.clarification.progressStageClarifyGate'),
    cache_lookup: t('nl2sql.clarification.progressStageCache'),
    generate_sql: t('nl2sql.clarification.progressStageGenerate'),
    done: t('nl2sql.clarification.progressStageDone'),
  };
  // Deduplicate labels so queued/request_validation don't render twice as the same chip.
  const stageDisplayOrder = stageOrder.filter((stage, idx, arr) =>
    idx === arr.findIndex((s) => stageLabelMap[s] === stageLabelMap[stage])
  );
  const stageIndexMap = stageOrder.reduce<Record<string, number>>((acc, stage, idx) => {
    acc[stage] = idx;
    return acc;
  }, {});
  const displayStageIndexMap = stageDisplayOrder.reduce<Record<string, number>>((acc, stage, idx) => {
    acc[stage] = idx;
    return acc;
  }, {});
  const currentStageIndex = progressStage ? stageOrder.indexOf(progressStage as typeof stageOrder[number]) : -1;
  const currentLabel = progressStage ? (stageLabelMap[progressStage] ?? progressStage) : null;
  const currentDisplayIndex = progressStage
    ? displayStageIndexMap[
      stageDisplayOrder[
        stageOrder.findIndex((s) => stageLabelMap[s] === (stageLabelMap[progressStage] ?? progressStage))
      ] ?? ''
    ]
    : -1;

  // New clarification turn => clear previous input so the textarea is always
  // for the NEXT补充, not stale text from the previous round.
  useEffect(() => {
    setFreeText('');
    setSelectedIndex(null);
  }, [context.turn, context.clarification_question]);

  const handleConfirm = () => {
    if (freeText.trim()) {
      onFreeText(freeText.trim());
    } else if (selectedIndex !== null) {
      const opt = context.options.find((o) => o.option_index === selectedIndex);
      if (opt) onSelect(opt);
    }
  };

  const handleFreeTextKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const nativeEvent = e.nativeEvent as KeyboardEvent & { keyCode?: number };
    const enteredRightAfterComposition =
      e.key === 'Enter' && Date.now() - lastCompositionEndAtRef.current < 80;
    const composing =
      composingRef.current
      || nativeEvent.isComposing
      || nativeEvent.keyCode === 229
      || enteredRightAfterComposition;
    if (composing) return;
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      if (freeText.trim()) {
        onFreeText(freeText.trim());
      }
    }
  };

  const quickSuggestionMap: Record<string, string[]> = {
    metric: ['指标看新用户数', '统计订单数', '按 GMV 计算'],
    time_range: ['最近7天', '本月', '昨天和今天'],
    granularity: ['按天统计', '按周汇总', '按月看趋势'],
    dimension: ['按租户分组', '按地区分组', '按产品分组'],
    filter: ['仅看华东', '排除退款订单', '只看企业客户'],
  };
  const quickSuggestions = Array.from(new Set(
    (context.missing_requirement_reasons ?? [])
      .flatMap((item) => quickSuggestionMap[item.key] ?? []),
  )).slice(0, 6);
  const isPlaceholderOption = (opt: ClarifyOption): boolean =>
    opt.table_name === '当前数据源' && opt.column_name === '补充需求';
  const selectableOptions = context.options.filter((opt) => !isPlaceholderOption(opt));
  const placeholderOption = context.options.find((opt) => isPlaceholderOption(opt));

  const handleQuickFill = (text: string) => {
    setSelectedIndex(null);
    setFreeText((prev) => {
      const trimmed = prev.trim();
      if (!trimmed) return text;
      if (trimmed.includes(text)) return trimmed;
      return `${trimmed}；${text}`;
    });
  };

  const hasInput = freeText.trim().length > 0 || selectedIndex !== null;

  return (
    <Card
      style={{
        background: 'rgba(245, 158, 11, 0.08)',
        border: '1px solid rgba(245, 158, 11, 0.45)',
        borderRadius: 12,
        marginBottom: 12,
      }}
      bodyStyle={{ padding: '16px 20px' }}
    >
      {/* Header */}
      <Space style={{ marginBottom: 12, display: 'flex', alignItems: 'flex-start' }}>
        <QuestionCircleOutlined style={{ color: '#f59e0b', fontSize: 20, marginTop: 2, flexShrink: 0 }} />
        <div style={{ flex: 1 }}>
          <Text strong style={{ fontSize: 14, color: '#fbbf24', display: 'block', marginBottom: 4 }}>
            {t('nl2sql.clarification.needMoreInfo')}
          </Text>
          <Text style={{ fontSize: 13, color: '#e5e7eb', lineHeight: 1.6 }}>
            {context.clarification_question}
          </Text>
        </div>
        {context.turn > 0 && (
          <Text style={{ fontSize: 11, color: 'rgba(251, 191, 36, 0.85)', flexShrink: 0 }}>
            {t('nl2sql.clarification.turn', { turn: context.turn })}
          </Text>
        )}
      </Space>

      {/* Original question */}
      <div style={{
        padding: '8px 12px',
        background: 'rgba(245, 158, 11, 0.14)',
        borderRadius: 8,
        marginBottom: 14,
        fontSize: 12,
        color: '#d1d5db',
        fontStyle: 'italic',
        borderLeft: '3px solid rgba(245, 158, 11, 0.6)',
      }}>
        {context.original_question}
      </div>

      {!!context.clarification_history?.length && (
        <div style={{ marginBottom: 12 }}>
          <Text style={{ fontSize: 12, color: '#cbd5e1', display: 'block', marginBottom: 8 }}>
            补充历史
          </Text>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {context.clarification_history.map((item) => (
              <div
                key={`clarify-history-${item.round}-${item.user_input}`}
                style={{
                  border: '1px solid rgba(148, 163, 184, 0.25)',
                  borderRadius: 8,
                  padding: '8px 10px',
                  background: 'rgba(15, 23, 42, 0.45)',
                }}
              >
                <Text style={{ fontSize: 11, color: '#a5b4fc', display: 'block', marginBottom: 4 }}>
                  Round {item.round}
                </Text>
                <Text style={{ fontSize: 12, color: '#e5e7eb', display: 'block' }}>
                  {item.user_input}
                </Text>
                {!!item.missing_after?.length && (
                  <Text style={{ fontSize: 11, color: '#fb923c', display: 'block', marginTop: 4 }}>
                    仍缺少：{item.missing_after.join('；')}
                  </Text>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {!!context.confirmed_requirements?.length && (
        <div style={{ marginBottom: 10 }}>
          <Text style={{ fontSize: 12, color: '#34d399', display: 'block', marginBottom: 6 }}>
            已确认条件
          </Text>
          <Space size={[6, 6]} wrap>
            {context.confirmed_requirements.map((item) => (
              <Tag
                key={`confirmed-${item}`}
                style={{
                  marginInlineEnd: 0,
                  color: '#a7f3d0',
                  background: 'rgba(16, 185, 129, 0.16)',
                  borderColor: 'rgba(52, 211, 153, 0.45)',
                }}
              >
                {item}
              </Tag>
            ))}
          </Space>
        </div>
      )}

      {!!context.missing_requirements?.length && (
        <div style={{ marginBottom: 12 }}>
          <Text style={{ fontSize: 12, color: '#fb923c', display: 'block', marginBottom: 6 }}>
            仍缺少条件
          </Text>
          <Space size={[6, 6]} wrap>
            {context.missing_requirements.map((item) => (
              <Tag
                key={`missing-${item}`}
                style={{
                  marginInlineEnd: 0,
                  color: '#fdba74',
                  background: 'rgba(249, 115, 22, 0.14)',
                  borderColor: 'rgba(251, 146, 60, 0.45)',
                }}
              >
                {item}
              </Tag>
            ))}
          </Space>
        </div>
      )}

      {!!context.missing_requirement_reasons?.length && (
        <div style={{ marginBottom: 12 }}>
          <Text style={{ fontSize: 12, color: '#fbbf24', display: 'block', marginBottom: 8 }}>
            {t('nl2sql.clarification.whyMissingTitle')}
          </Text>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {context.missing_requirement_reasons.map((item) => (
              <div
                key={`missing-reason-${item.key}-${item.requirement}`}
                style={{
                  border: '1px solid rgba(251, 191, 36, 0.32)',
                  borderRadius: 8,
                  padding: '8px 10px',
                  background: 'rgba(245, 158, 11, 0.12)',
                }}
              >
                <Text style={{ fontSize: 12, color: '#fcd34d', display: 'block', marginBottom: 4 }}>
                  {item.requirement}
                </Text>
                <Text style={{ fontSize: 12, color: '#e5e7eb', display: 'block', marginBottom: 3 }}>
                  {t('nl2sql.clarification.whyMissingLabel')}：{item.why_missing}
                </Text>
                <Text style={{ fontSize: 12, color: '#cbd5e1', display: 'block' }}>
                  {t('nl2sql.clarification.howToProvideLabel')}：{item.how_to_provide}
                </Text>
                {!!item.examples?.length && (
                  <Text style={{ fontSize: 11, color: '#93c5fd', display: 'block', marginTop: 4 }}>
                    {t('nl2sql.clarification.examplesLabel')}：{item.examples.join('；')}
                  </Text>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {!!quickSuggestions.length && (
        <div style={{ marginBottom: 12 }}>
          <Text style={{ fontSize: 12, color: '#93c5fd', display: 'block', marginBottom: 6 }}>
            {t('nl2sql.clarification.quickFillTitle')}
          </Text>
          <Space size={[6, 6]} wrap>
            {quickSuggestions.map((item) => (
              <Button
                key={`quick-fill-${item}`}
                size="small"
                onClick={() => handleQuickFill(item)}
                disabled={loading}
                style={{
                  borderColor: 'rgba(96, 165, 250, 0.45)',
                  color: '#bfdbfe',
                  background: 'rgba(59, 130, 246, 0.12)',
                  fontSize: 11,
                }}
              >
                {item}
              </Button>
            ))}
          </Space>
        </div>
      )}

      {/* Options */}
      {!!selectableOptions.length && (
        <Radio.Group
          value={selectedIndex}
          onChange={(e) => setSelectedIndex(e.target.value as number)}
          disabled={loading}
          style={{ display: 'flex', flexDirection: 'column', gap: 8, width: '100%', marginBottom: 12 }}
        >
          {selectableOptions.map((opt) => (
            <Radio.Button
              key={opt.option_index}
              value={opt.option_index}
              style={{
                height: 'auto',
                padding: '12px 16px',
                borderRadius: 8,
                border: selectedIndex === opt.option_index ? '2px solid #f59e0b' : '1px solid #e5e7eb',
                borderColor: selectedIndex === opt.option_index ? '#f59e0b' : 'rgba(148, 163, 184, 0.35)',
                textAlign: 'left',
                display: 'flex',
                flexDirection: 'column',
                gap: 2,
                width: '100%',
                background: selectedIndex === opt.option_index ? 'rgba(245, 158, 11, 0.14)' : 'rgba(17, 24, 39, 0.55)',
                lineHeight: 1.4,
              }}
            >
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', width: '100%' }}>
                <Text strong style={{ fontSize: 13, color: '#f9fafb' }}>
                  {opt.table_name}.{opt.column_name}
                </Text>
                {opt.sim_score > 0 && (
                  <Text style={{ fontSize: 11, color: '#c4b5fd', flexShrink: 0, marginLeft: 8 }}>
                    {Math.round(opt.sim_score * 100)}%
                  </Text>
                )}
              </div>
              {opt.business_meaning && (
                <Text style={{ fontSize: 12, color: '#cbd5e1', marginTop: 2 }}>
                  {opt.business_meaning}
                </Text>
              )}
              {opt.reason && (
                <Text style={{ fontSize: 11, color: '#94a3b8', fontStyle: 'italic', marginTop: 2 }}>
                  {opt.reason}
                </Text>
              )}
            </Radio.Button>
          ))}
        </Radio.Group>
      )}

      {!selectableOptions.length && !!placeholderOption && (
        <div
          style={{
            marginBottom: 12,
            border: '1px dashed rgba(148, 163, 184, 0.45)',
            borderRadius: 8,
            padding: '8px 10px',
            background: 'rgba(15, 23, 42, 0.4)',
          }}
        >
          <Text style={{ fontSize: 12, color: '#cbd5e1' }}>
            {placeholderOption.business_meaning || '请在下方输入框补充缺失条件。'}
          </Text>
        </div>
      )}

      {/* Free text fallback */}
      <div style={{ marginBottom: 12 }}>
        <Text style={{ fontSize: 12, color: '#cbd5e1', display: 'block', marginBottom: 6 }}>
          {t('nl2sql.clarification.orType')}
        </Text>
        <Input.TextArea
          value={freeText}
          onChange={(e) => { setFreeText(e.target.value); setSelectedIndex(null); }}
          onCompositionStart={() => {
            composingRef.current = true;
          }}
          onCompositionEnd={() => {
            composingRef.current = false;
            lastCompositionEndAtRef.current = Date.now();
          }}
          onKeyDown={handleFreeTextKeyDown}
          disabled={loading}
          placeholder={t('nl2sql.clarification.freeTextPlaceholder')}
          rows={2}
          style={{
            fontSize: 13,
            color: '#f3f4f6',
            background: 'rgba(15, 23, 42, 0.65)',
            borderColor: 'rgba(148, 163, 184, 0.35)',
          }}
        />
      </div>

      {/* Actions */}
      <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
        <Button
          onClick={() => { onCancel?.(); }}
          disabled={loading}
        >
          {t('nl2sql.clarification.cancel')}
        </Button>
        <Button
          type="primary"
          onClick={handleConfirm}
          disabled={loading || !hasInput}
          loading={loading}
          style={{ minWidth: 80 }}
        >
          {t('nl2sql.clarification.confirm')}
        </Button>
      </div>
      {loading && (
        <div style={{ marginTop: 10 }}>
          <Text style={{ fontSize: 11, color: '#93c5fd', display: 'block', marginBottom: 6 }}>
            {currentLabel
              ? `${t('nl2sql.clarification.progressTitle')} · ${currentLabel}`
              : t('nl2sql.clarification.progressTitle')}
          </Text>
          <Space size={[6, 6]} wrap>
            {stageDisplayOrder.map((stage, idx) => {
              const stageRawIndex = stageIndexMap[stage];
              const completed = currentStageIndex >= 0 ? stageRawIndex < currentStageIndex : false;
              const active = currentDisplayIndex >= 0 ? idx === currentDisplayIndex : idx === 0;
              return (
                <Tag
                  key={`clarify-progress-${stage}-${stageLabelMap[stage]}`}
                  style={{
                    marginInlineEnd: 0,
                    color: completed ? '#86efac' : active ? '#67e8f9' : '#94a3b8',
                    background: completed
                      ? 'rgba(34, 197, 94, 0.16)'
                      : active
                        ? 'rgba(6, 182, 212, 0.18)'
                        : 'rgba(51, 65, 85, 0.22)',
                    borderColor: completed
                      ? 'rgba(134, 239, 172, 0.5)'
                      : active
                        ? 'rgba(34, 211, 238, 0.45)'
                        : 'rgba(148, 163, 184, 0.22)',
                    fontSize: 11,
                    fontWeight: completed || active ? 600 : 500,
                  }}
                >
                  {completed ? `✅ ${stageLabelMap[stage]}` : stageLabelMap[stage]}
                </Tag>
              );
            })}
          </Space>
        </div>
      )}
    </Card>
  );
}
