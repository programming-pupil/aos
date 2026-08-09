import { useEffect, useMemo, useState } from 'react';
import { Alert, Button, Card, Checkbox, Empty, Space, Spin, Tag, Tooltip, Typography } from 'antd';
import { RollbackOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { RdFileChange } from '@/types';
import { isRdFileChangeApplicable, rdFileChangeNotApplicableReason } from '@/utils/rdChanges';
import {
  DIFF_COLLAPSE_LINE_LIMIT,
  DIFF_PREVIEW_HEAD_LINES,
  DIFF_PREVIEW_TAIL_LINES,
  RD_RISK_LEVEL_COLORS,
} from './constants';
import type { ParsedDiffHunk, RdRiskFile, RdRiskLevel } from './types';

const { Text } = Typography;

function parseDiffHunks(diff: string): ParsedDiffHunk[] {
  const hunks: ParsedDiffHunk[] = [];
  let current: ParsedDiffHunk | null = null;
  for (const line of diff.split('\n')) {
    if (line.startsWith('@@')) {
      if (current) hunks.push(current);
      current = {
        index: hunks.length,
        title: line,
        lines: [line],
      };
    } else if (current) {
      current.lines.push(line);
    }
  }
  if (current) hunks.push(current);
  return hunks;
}

function DiffBlock({ change }: { change: RdFileChange }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const lines = useMemo(() => change.diffPatch.split('\n'), [change.diffPatch]);
  const shouldCollapse = lines.length > DIFF_COLLAPSE_LINE_LIMIT;
  const visibleLines = useMemo(() => {
    if (!shouldCollapse || expanded) return lines;
    return [
      ...lines.slice(0, DIFF_PREVIEW_HEAD_LINES),
      `... ${t('rd.diffHiddenLines', '已折叠 {{count}} 行 Diff，点击展开查看完整内容', {
        count: Math.max(0, lines.length - DIFF_PREVIEW_HEAD_LINES - DIFF_PREVIEW_TAIL_LINES),
      })} ...`,
      ...lines.slice(-DIFF_PREVIEW_TAIL_LINES),
    ];
  }, [expanded, lines, shouldCollapse, t]);

  return (
    <Space direction="vertical" size={8} style={{ width: '100%' }}>
      {shouldCollapse ? (
        <Alert
          type="info"
          showIcon
          message={t('rd.largeDiffPreview', '大 Diff 已启用预览渲染')}
          description={t('rd.largeDiffPreviewDesc', '为避免详情区域卡顿，默认只渲染头尾关键行；需要审查完整内容时再展开。')}
          action={(
            <Button size="small" onClick={() => setExpanded((value) => !value)}>
              {expanded ? t('common.collapse', '收起') : t('rd.expandFullDiff', '展开完整 Diff')}
            </Button>
          )}
        />
      ) : null}
      <pre
        style={{
          margin: 0,
          padding: 14,
          borderRadius: 14,
          overflow: 'auto',
          maxHeight: 460,
          background: '#07111f',
          border: '1px solid rgba(148, 163, 184, 0.22)',
          color: '#dbeafe',
          fontFamily: 'var(--font-code, "JetBrains Mono", monospace)',
          fontSize: 12,
          lineHeight: 1.65,
        }}
      >
        {visibleLines.map((line, idx) => {
          const color = line.startsWith('+') && !line.startsWith('+++')
            ? '#86efac'
            : line.startsWith('-') && !line.startsWith('---')
              ? '#fca5a5'
              : line.startsWith('@@')
                ? '#93c5fd'
                : line.startsWith('...')
                  ? '#facc15'
                  : '#dbeafe';
          return (
            <div key={`${idx}-${line.slice(0, 12)}`} style={{ color }}>
              {line || ' '}
            </div>
          );
        })}
      </pre>
    </Space>
  );
}

function HunkReview({
  change,
  disabled,
  loading,
  onApplyHunks,
}: {
  change: RdFileChange;
  disabled?: boolean;
  loading?: boolean;
  onApplyHunks: (change: RdFileChange, hunkIndexes: number[]) => void;
}) {
  const { t } = useTranslation();
  const hunks = useMemo(() => parseDiffHunks(change.diffPatch), [change.diffPatch]);
  const [selected, setSelected] = useState<number[]>(() => hunks.map((hunk) => hunk.index));

  useEffect(() => {
    setSelected(hunks.map((hunk) => hunk.index));
  }, [change.id, hunks]);

  if (change.applied || hunks.length <= 1) {
    return <DiffBlock change={change} />;
  }

  const selectedSet = new Set(selected);
  return (
    <Space direction="vertical" size={10} style={{ width: '100%' }}>
      <Space wrap style={{ justifyContent: 'space-between', width: '100%' }}>
        <Text style={{ color: '#94a3b8' }}>
          {t('rd.selectedHunks', '已选择 {{selected}} / {{total}} 个修改块', {
            selected: selected.length,
            total: hunks.length,
          })}
        </Text>
        <Space>
          <Button size="small" onClick={() => setSelected(hunks.map((hunk) => hunk.index))}>
            {t('rd.selectAllHunks', '全选')}
          </Button>
          <Button size="small" onClick={() => setSelected([])}>
            {t('rd.clearHunks', '清空')}
          </Button>
          <Button
            size="small"
            type="primary"
            disabled={disabled || selected.length === 0}
            loading={loading}
            onClick={() => onApplyHunks(change, selected)}
          >
            {t('rd.applySelectedHunks', '应用选中块')}
          </Button>
        </Space>
      </Space>
      {hunks.map((hunk) => (
        <Card
          key={`${change.id}-${hunk.index}`}
          size="small"
          style={{
            background: selectedSet.has(hunk.index) ? 'rgba(20, 184, 166, 0.08)' : 'rgba(2, 6, 23, 0.36)',
            borderColor: selectedSet.has(hunk.index) ? 'rgba(45, 212, 191, 0.45)' : 'rgba(148, 163, 184, 0.14)',
          }}
          title={
            <Checkbox
              checked={selectedSet.has(hunk.index)}
              onChange={(event) => {
                setSelected((prev) => (
                  event.target.checked
                    ? Array.from(new Set([...prev, hunk.index])).sort((a, b) => a - b)
                    : prev.filter((idx) => idx !== hunk.index)
                ));
              }}
            >
              <Text style={{ color: '#e2e8f0' }}>
                {t('rd.hunkTitle', '修改块 #{{index}}', { index: hunk.index + 1 })}
              </Text>
            </Checkbox>
          }
        >
          <pre
            style={{
              margin: 0,
              maxHeight: 300,
              overflow: 'auto',
              whiteSpace: 'pre-wrap',
              color: '#dbeafe',
              fontFamily: 'var(--font-code, "JetBrains Mono", monospace)',
              fontSize: 12,
              lineHeight: 1.6,
            }}
          >
            {hunk.lines.join('\n')}
          </pre>
        </Card>
      ))}
    </Space>
  );
}

export function DiffInspector({
  changes,
  loading,
  compact = false,
  hasPendingChanges,
  hasAppliedChanges,
  canApply,
  applyLoading,
  rollbackLoading,
  rollbackVariables,
  applyHunksLoading,
  applyHunksChangeId,
  riskByPath,
  riskLevelLabel,
  onApply,
  onRollback,
  onApplyHunks,
}: {
  changes: RdFileChange[];
  loading?: boolean;
  compact?: boolean;
  hasPendingChanges: boolean;
  hasAppliedChanges: boolean;
  canApply: boolean;
  applyLoading?: boolean;
  rollbackLoading?: boolean;
  rollbackVariables?: string[];
  applyHunksLoading?: boolean;
  applyHunksChangeId?: string;
  riskByPath: Map<string, RdRiskFile>;
  riskLevelLabel: (level: RdRiskLevel) => string;
  onApply: (change?: RdFileChange) => void;
  onRollback: (change?: RdFileChange) => void;
  onApplyHunks: (change: RdFileChange, hunkIndexes: number[]) => void;
}) {
  const { t } = useTranslation();
  const riskForChange = (change: RdFileChange) => (
    riskByPath.get(change.filePath)
    ?? Array.from(riskByPath.values()).find((file) => file.path.endsWith(change.filePath) || change.filePath.endsWith(file.path))
  );

  if (loading) {
    return (
      <div style={{ padding: compact ? '24px 0' : '32px 0', textAlign: 'center' }}>
        <Spin />
        <div style={{ marginTop: 10 }}>
          <Text style={{ color: '#94a3b8' }}>{t('rd.diffLoading', '正在加载 Diff...')}</Text>
        </div>
      </div>
    );
  }
  if (changes.length === 0) {
    return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.noChanges', '暂无 Diff')}</span>} />;
  }

  return (
    <Space direction="vertical" size={12} style={{ width: '100%' }}>
      <Alert
        type={hasPendingChanges ? 'warning' : 'success'}
        showIcon
        message={hasPendingChanges
          ? t('rd.pendingDiffReviewTitle', '已生成待审批 Diff')
          : t('rd.diffReviewDoneTitle', 'Diff 已处理')}
        description={t('rd.pendingDiffReviewDesc', '这里是本轮 Agent 产出的真实补丁。主仓库不会被静默修改，只有点击应用后才会写入。')}
        action={(
          <Space wrap>
            {hasAppliedChanges ? (
              <Button
                size="small"
                icon={<RollbackOutlined />}
                disabled={!canApply || rollbackLoading}
                loading={rollbackLoading && !rollbackVariables}
                onClick={() => onRollback()}
              >
                {t('rd.rollbackAll', '回滚全部')}
              </Button>
            ) : null}
            <Button
              size="small"
              danger
              type={hasPendingChanges ? 'primary' : 'default'}
              disabled={!canApply || !hasPendingChanges || applyLoading}
              loading={applyLoading}
              onClick={() => onApply()}
            >
              {t('rd.applyAll', '应用全部')}
            </Button>
          </Space>
        )}
      />
      <Space direction="vertical" size={12} style={{ width: '100%' }}>
        {changes.map((change) => {
          const notApplicableReason = rdFileChangeNotApplicableReason(change);
          const applicable = isRdFileChangeApplicable(change);
          const fileRisk = riskForChange(change);
          return (
            <Card
              key={change.id}
              size="small"
              title={(
                <Space wrap>
                  <Text style={{ color: '#e2e8f0' }}>{change.filePath}</Text>
                  {change.applied ? <Tag color="success">{t('rd.applied', '已应用')}</Tag> : <Tag color="warning">{t('rd.pendingApply', '待应用')}</Tag>}
                  {notApplicableReason ? <Tag color="default">{t('rd.notApplicable', '不可应用')}</Tag> : null}
                  {fileRisk ? <Tag color={RD_RISK_LEVEL_COLORS[fileRisk.riskLevel]}>{riskLevelLabel(fileRisk.riskLevel)}</Tag> : null}
                  {fileRisk?.lineHints?.slice(0, 3).map((line) => <Tag key={`${change.id}-L${line}`}>L{line}</Tag>)}
                </Space>
              )}
              extra={
                <Space>
                  {change.applied ? (
                    <Button
                      size="small"
                      icon={<RollbackOutlined />}
                      disabled={!canApply || rollbackLoading}
                      loading={rollbackLoading && rollbackVariables?.length === 1 && rollbackVariables[0] === change.id}
                      onClick={() => onRollback(change)}
                    >
                      {t('rd.rollbackPatch', '回滚修改')}
                    </Button>
                  ) : (
                    <Tooltip title={notApplicableReason ? t('rd.notApplicableHint', '该 Diff 已失效、被新补丁替代，或包含 AOS 内部运行时路径') : undefined}>
                      <Button size="small" disabled={!canApply || !applicable || applyLoading} onClick={() => onApply(change)}>
                        {t('rd.applyPatch', '应用修改')}
                      </Button>
                    </Tooltip>
                  )}
                </Space>
              }
              style={{ background: 'rgba(2, 6, 23, 0.38)', borderColor: fileRisk ? 'rgba(251, 191, 36, 0.26)' : 'rgba(148, 163, 184, 0.16)' }}
            >
              <HunkReview
                change={change}
                disabled={!canApply || !applicable || applyHunksLoading}
                loading={applyHunksLoading && applyHunksChangeId === change.id}
                onApplyHunks={onApplyHunks}
              />
              {!compact && fileRisk?.reasons?.length ? (
                <div style={{ marginTop: 10, color: '#cbd5e1', fontSize: 12 }}>
                  {fileRisk.reasons.slice(0, 3).map((reason) => <div key={`${change.id}-${reason}`}>- {reason}</div>)}
                </div>
              ) : null}
            </Card>
          );
        })}
      </Space>
    </Space>
  );
}
