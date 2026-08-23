// ── NL2SQL — natural language to SQL for business and analysts ─────────────────────────
// Conversational interface: select data source, ask questions in natural language,
// view generated SQL, execute, and see tabular results. Full multi-turn context support.
//
// Design principles:
//  1. Multi-turn: each query remembers context from previous turns in the same thread.
//  2. Result-first: generate SQL, execute it automatically, and surface analysis first.
//  3. Smart source selection: user picks manually OR AI infers the best source from NL.
//  4. Progressive disclosure: schema panel shows tables/columns, not raw JSON.
//  5. SQL remains editable/auditable for analysts.
//  6. Saved views: query results can be saved as named views for reuse.

import { lazy, Suspense, useState, useRef, useCallback, useMemo, useEffect } from 'react';
import type { ComponentType, ElementRef } from 'react';
import type { MouseEvent as ReactMouseEvent } from 'react';
import {
  Layout,
  Typography, Button, Input, Space, message, Tag, Tooltip, Switch,
  Table, Card, Drawer, Spin, Empty, Divider, Select, Collapse,
  Badge, Popover, Segmented, Dropdown, Modal, Pagination, Alert, Popconfirm,
  Checkbox, Upload,
} from 'antd';
import type { UploadProps } from 'antd';
import { ClarificationCard } from '@/components/nl2sql/ClarificationCard';
import type { ClarificationContext as ClarificationContextType } from '@/components/nl2sql/ClarificationCard';
import { QueryUnderstandingPanel } from '@/components/nl2sql/QueryUnderstandingPanel';
import { ExplainTab } from '@/components/nl2sql/ExplainTab';
import { SpellCorrection, SemanticUnreachable } from '@/components/nl2sql/SpellCorrection';
import { PromptQueuePanel } from '@/components/PromptQueuePanel';
import type { QueryUnderstandingResponse } from '@/types';
import type { MenuProps } from 'antd';
import {
  SendOutlined, DatabaseOutlined, PlayCircleOutlined, CopyOutlined,
  CheckOutlined, LoadingOutlined, QuestionCircleOutlined, TableOutlined,
  RobotOutlined, UserOutlined,
  SyncOutlined, DownloadOutlined, SaveOutlined, ThunderboltOutlined,
  LineChartOutlined, BarChartOutlined, PieChartOutlined, StarOutlined,
  EditOutlined, CloseOutlined, CheckCircleOutlined, ExclamationCircleOutlined, ShareAltOutlined,
  InfoCircleOutlined, DownOutlined, PlusOutlined, UpOutlined,
  RightOutlined, LeftOutlined, CommentOutlined, MessageOutlined, FileTextOutlined,
  LikeOutlined, DislikeOutlined, DeleteOutlined,
  PaperClipOutlined, UploadOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi, streamNl2sqlAgentTask, streamNl2sqlClarifyTask, streamNl2sqlQueryTask, streamNl2sqlRouteTask } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { Markdown } from '@/components/chat/markdownRenderer';
import type {
  AppliedRuleHit,
  DataSourceInfo,
  Nl2sqlQueryResponse,
  Nl2sqlQueryTaskEvent,
  Nl2sqlReferenceBindings,
  Nl2sqlReferencePack,
  Nl2sqlReferenceUsage,
} from '@/types';
import type {
  AgentStepResult,
  EditableView,
  NlTurn,
  QueryResult,
  ChartType,
  QueryStageTimelineItem,
  SchemaColumn,
  SchemaTable,
  ViewTab,
} from './nl2sql/types';
import { ApiError } from '@/api/errors';
import dayjs from 'dayjs';
import i18n from 'i18next';
import relativeTime from 'dayjs/plugin/relativeTime';
import { useTranslation } from 'react-i18next';
import { useNavigate } from '@/router';
import Editor from '@monaco-editor/react';
import { ErrorBoundary } from '@/components/ErrorBoundary';
import { usePromptQueue } from '@/hooks/usePromptQueue';
import { useDismissibleNotice } from '@/hooks/useDismissibleNotice';
import { downloadCSV, downloadExcel, downloadJSON } from './nl2sql/downloads';
import {
  appendMultiSourceStepTimeline,
  appendOrUpdateStageTimeline,
  buildSemanticContextFromMatchedTables,
  fallbackGranularityLabel,
  formatDuration,
  formatTime,
  getRouteStageText,
  mapConversationMessagesToTurns,
  mergeAppliedRules,
  normalizeNl2sqlErrorMessage,
  parseSqlError,
  resolveMultiSourceResult,
  resolveRouteStageMessage,
  resolveTurnQueryId,
} from './nl2sql/helpers';
import type { ChartPanelProps } from '@/components/nl2sql/ChartPanel';

dayjs.extend(relativeTime);

const { Text, Title } = Typography;
const { TextArea } = Input;
const { Panel } = Collapse;

const LazyChartPanel = lazy(() => import('@/components/nl2sql/ChartPanel').then((mod) => ({ default: mod.ChartPanel as ComponentType<ChartPanelProps> })));
const EMPTY_REFERENCE_PACKS: Nl2sqlReferencePack[] = [];

function dataSourceTables(dataSource?: DataSourceInfo | null) {
  const schema = dataSource?.schema_info as unknown;
  if (Array.isArray(schema)) return schema;
  if (schema && typeof schema === 'object') {
    const tables = (schema as { tables?: unknown }).tables;
    if (Array.isArray(tables)) return tables;
  }
  return [];
}

function isStandaloneGreeting(input: string) {
    const normalized = input.trim().toLocaleLowerCase().replace(/[!！。,.，?？\s]+/g, '');
    return /^(你好|您好|嗨|哈喽|hello|hi|hey|在吗|早上好|下午好|晚上好)$/.test(normalized);
}

function isPunctuationOnly(input: string) {
  const value = input.trim();
  return value.length > 0 && /^[\p{P}\p{S}\s]+$/u.test(value);
}

function RuleHitsPanel({
  rules,
  t,
}: {
  rules?: AppliedRuleHit[];
  t: ReturnType<typeof useTranslation>['t'];
}) {
  const [expanded, setExpanded] = useState(false);
  const list = rules ?? [];
  if (list.length === 0) return null;

  return (
    <div style={{ marginTop: 8 }}>
      <Button
        size="small"
        type="text"
        onClick={() => setExpanded((v) => !v)}
        style={{ paddingInline: 6, height: 22, fontSize: 11, color: 'var(--text-secondary)' }}
        icon={expanded ? <UpOutlined /> : <DownOutlined />}
      >
        {t('nl2sql.ruleHits.toggle', { count: list.length })}
      </Button>
      {expanded && (
        <div style={{
          marginTop: 6,
          padding: '8px 10px',
          borderRadius: 8,
          border: '1px solid var(--border-subtle)',
          background: 'var(--bg-elevated)',
          display: 'grid',
          gap: 4,
        }}>
          {list.map((rule, idx) => (
            <div
              key={`${rule.ruleKey}-${idx}`}
              style={{ display: 'flex', alignItems: 'flex-start', gap: 6, fontSize: 11, color: 'var(--text-secondary)' }}
            >
              <Tag style={{ marginInlineEnd: 0, fontSize: 10, lineHeight: '16px' }}>{rule.ruleName}</Tag>
              {rule.detail && (
                <span style={{ lineHeight: 1.5 }}>{rule.detail}</span>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

type ReferenceFileGroup = {
  key: string;
  packName: string;
  filename: string;
  language?: string | null;
  verified: boolean;
  references: Nl2sqlReferenceUsage[];
  primary: Nl2sqlReferenceUsage;
  ranges: string[];
  chunkTypes: string[];
  reasons: string[];
  score: number;
};

function groupReferenceUsage(references: Nl2sqlReferenceUsage[]): ReferenceFileGroup[] {
  const grouped = new Map<string, Nl2sqlReferenceUsage[]>();
  references.forEach((ref) => {
    const key = `${ref.packId}:${ref.fileId || ref.filename}`;
    grouped.set(key, [...(grouped.get(key) ?? []), ref]);
  });

  return Array.from(grouped.entries())
    .map(([key, refs]) => {
      const sorted = [...refs].sort((a, b) => a.startLine - b.startLine || b.score - a.score);
      const primary = sorted.reduce((best, ref) => (ref.score > best.score ? ref : best), sorted[0]);
      const ranges = Array.from(new Set(sorted.map((ref) => `L${ref.startLine}-${ref.endLine}`)));
      const chunkTypes = Array.from(new Set(sorted.map((ref) => ref.chunkType).filter((value): value is string => Boolean(value))));
      const reasons = Array.from(new Set(sorted.map((ref) => ref.reason).filter(Boolean)));
      return {
        key,
        packName: primary.packName,
        filename: primary.filename,
        language: primary.language ?? sorted.find((ref) => ref.language)?.language,
        verified: sorted.some((ref) => ref.verified),
        references: sorted,
        primary,
        ranges,
        chunkTypes,
        reasons,
        score: Math.max(...sorted.map((ref) => ref.score)),
      };
    })
    .sort((a, b) => b.score - a.score || a.filename.localeCompare(b.filename));
}

function ReferenceHitsPanel({
  references,
  t,
}: {
  references?: Nl2sqlReferenceUsage[];
  t: ReturnType<typeof useTranslation>['t'];
}) {
  const [expanded, setExpanded] = useState(false);
  const list = references ?? [];
  const groups = useMemo(() => groupReferenceUsage(list), [list]);
  const fragmentCount = groups.reduce((sum, group) => sum + group.references.length, 0);
  if (list.length === 0) return null;

  return (
    <div style={{ marginTop: 8 }}>
      <Button
        size="small"
        type="text"
        onClick={() => setExpanded((v) => !v)}
        style={{ paddingInline: 6, height: 22, fontSize: 11, color: 'var(--text-secondary)' }}
        icon={expanded ? <UpOutlined /> : <DownOutlined />}
      >
        {t('nl2sql.references.usedToggleGrouped', { files: groups.length, chunks: fragmentCount })}
      </Button>
      {expanded && (
        <div style={{
          marginTop: 6,
          padding: '8px 10px',
          borderRadius: 8,
          border: '1px solid var(--border-subtle)',
          background: 'var(--bg-elevated)',
          display: 'grid',
          gap: 8,
        }}>
          {groups.map((group) => (
            <div key={group.key} style={{ minWidth: 0 }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap', marginBottom: 4 }}>
                <Tag color="blue" style={{ marginInlineEnd: 0, fontSize: 10 }}>{group.packName}</Tag>
                <Text style={{ fontSize: 11, color: 'var(--text-primary)', maxWidth: 260 }} ellipsis={{ tooltip: group.filename }}>
                  {group.filename}
                </Text>
                {group.references.length > 1 && (
                  <Tag style={{ marginInlineEnd: 0, fontSize: 10 }}>
                    {t('nl2sql.references.fragmentCount', { count: group.references.length })}
                  </Tag>
                )}
                {group.ranges.slice(0, 3).map((range) => (
                  <Text key={range} style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                    {range}
                  </Text>
                ))}
                {group.ranges.length > 3 && (
                  <Tag style={{ marginInlineEnd: 0, fontSize: 10 }}>
                    {t('nl2sql.references.moreRanges', { count: group.ranges.length - 3 })}
                  </Tag>
                )}
                {group.chunkTypes.slice(0, 2).map((chunkType) => (
                  <Tag key={chunkType} style={{ marginInlineEnd: 0, fontSize: 10 }}>{chunkType}</Tag>
                ))}
                {group.language && <Tag style={{ marginInlineEnd: 0, fontSize: 10 }}>{group.language}</Tag>}
                {group.verified && <Tag color="green" style={{ marginInlineEnd: 0, fontSize: 10 }}>{t('sqlKnowledge.verified')}</Tag>}
              </div>
              <div style={{
                fontSize: 11,
                color: 'var(--text-secondary)',
                lineHeight: 1.55,
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-word',
                maxHeight: 96,
                overflow: 'auto',
                padding: '6px 8px',
                borderRadius: 6,
                background: 'var(--bg-secondary)',
              }}>
                {group.primary.preview}
              </div>
              <Text style={{ display: 'block', marginTop: 4, fontSize: 10, color: 'var(--text-muted)' }}>
                {group.reasons.slice(0, 2).join(' / ')} · {t('nl2sql.references.score', { score: group.score.toFixed(2) })}
              </Text>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function ReferenceBindingPopover({
  packs,
  selectedPackIds,
  selectedFileIds,
  includeAll,
  loading,
  uploadingPackId,
  onToggleAll,
  onChangePacks,
  onChangeFiles,
  onCreatePack,
  onUploadFile,
  onTogglePackEnabled,
  onDeletePack,
  onDeleteFile,
  t,
}: {
  packs: Nl2sqlReferencePack[];
  selectedPackIds: string[];
  selectedFileIds: string[];
  includeAll: boolean;
  loading: boolean;
  uploadingPackId: string | null;
  onToggleAll: (checked: boolean) => void;
  onChangePacks: (ids: string[]) => void;
  onChangeFiles: (ids: string[]) => void;
  onCreatePack: (name: string) => void;
  onUploadFile: (packId: string, file: File) => void;
  onTogglePackEnabled: (pack: Nl2sqlReferencePack) => void;
  onDeletePack: (pack: Nl2sqlReferencePack) => void;
  onDeleteFile: (fileId: string) => void;
  t: ReturnType<typeof useTranslation>['t'];
}) {
  const [newPackName, setNewPackName] = useState('');

  const allPackIds = packs.map((pack) => pack.id);
  const allFileIds = packs.flatMap((pack) => pack.files.map((file) => file.id));
  const activePackIds = selectedPackIds.filter((id) => allPackIds.includes(id));
  const activeFileIds = selectedFileIds.filter((id) => allFileIds.includes(id));

  const uploadPropsFor = (packId: string): UploadProps => ({
    showUploadList: false,
    beforeUpload: (file) => {
      onUploadFile(packId, file);
      return false;
    },
    disabled: false,
  });

  return (
    <div style={{ width: 420, maxWidth: 'calc(100vw - 48px)' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, marginBottom: 10 }}>
        <div>
          <Text strong style={{ fontSize: 13 }}>{t('nl2sql.references.title')}</Text>
          <Text style={{ display: 'block', fontSize: 11, color: 'var(--text-muted)' }}>
            {t('nl2sql.references.hint')}
          </Text>
        </div>
        <Switch
          size="small"
          checked={includeAll}
          onChange={onToggleAll}
          checkedChildren={t('nl2sql.references.allShort')}
          unCheckedChildren={t('nl2sql.references.selectShort')}
        />
      </div>

      <div style={{ display: 'flex', gap: 6, marginBottom: 10 }}>
        <Input
          size="small"
          value={newPackName}
          onChange={(e) => setNewPackName(e.target.value)}
          onPressEnter={() => {
            const name = newPackName.trim();
            if (!name) return;
            onCreatePack(name);
            setNewPackName('');
          }}
          placeholder={t('nl2sql.references.newPackPlaceholder')}
        />
        <Button
          size="small"
          icon={<PlusOutlined />}
          onClick={() => {
            const name = newPackName.trim();
            if (!name) return;
            onCreatePack(name);
            setNewPackName('');
          }}
        >
          {t('common.create')}
        </Button>
      </div>

      <Alert
        type="info"
        showIcon
        icon={<InfoCircleOutlined />}
        message={t('nl2sql.references.uploadGuideTitle')}
        description={t('nl2sql.references.uploadGuide')}
        style={{ marginBottom: 10, padding: '6px 10px', fontSize: 11 }}
      />

      {loading ? (
        <div style={{ padding: 24, textAlign: 'center' }}><Spin size="small" /></div>
      ) : packs.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={
            <Space direction="vertical" size={2}>
              <Text style={{ fontSize: 12 }}>{t('nl2sql.references.empty')}</Text>
              <Text type="secondary" style={{ fontSize: 11 }}>
                {t('nl2sql.references.emptyHint')}
              </Text>
            </Space>
          }
        />
      ) : (
        <div style={{ maxHeight: 360, overflow: 'auto', paddingRight: 2 }}>
          <Checkbox
            checked={includeAll}
            onChange={(e) => onToggleAll(e.target.checked)}
            style={{ marginBottom: 8 }}
          >
            {t('nl2sql.references.includeAll')}
          </Checkbox>
          <Checkbox.Group
            disabled={includeAll}
            value={activePackIds}
            onChange={(values) => onChangePacks(values.map(String))}
            style={{ width: '100%', display: 'grid', gap: 8 }}
          >
            {packs.map((pack) => (
              <div
                key={pack.id}
                style={{
                  border: '1px solid var(--border-subtle)',
                  borderRadius: 8,
                  padding: 8,
                  background: pack.enabled ? 'var(--bg-secondary)' : 'var(--bg-tertiary)',
                  opacity: pack.enabled ? 1 : 0.72,
                }}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <Checkbox value={pack.id} disabled={!pack.enabled} />
                  <div style={{ minWidth: 0, flex: 1 }}>
                    <Text
                      style={{ fontSize: 12, maxWidth: 200 }}
                      ellipsis={{ tooltip: pack.name }}
                      delete={!pack.enabled}
                    >
                      {pack.name}
                    </Text>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 4, flexWrap: 'wrap' }}>
                      <Text style={{ display: 'block', fontSize: 10, color: 'var(--text-muted)' }}>
                        {t('nl2sql.references.packStats', { files: pack.fileCount, chunks: pack.chunkCount })}
                      </Text>
                      {!pack.enabled && (
                        <Tag style={{ marginInlineEnd: 0, fontSize: 9 }}>{t('nl2sql.references.disabled')}</Tag>
                      )}
                    </div>
                  </div>
                  <Upload {...uploadPropsFor(pack.id)}>
                    <Button
                      size="small"
                      type="text"
                      loading={uploadingPackId === pack.id}
                      icon={<UploadOutlined />}
                      disabled={!pack.enabled}
                    >
                      {t('nl2sql.references.uploadFile')}
                    </Button>
                  </Upload>
                  <Tooltip title={pack.enabled ? t('nl2sql.references.disablePack') : t('nl2sql.references.enablePack')}>
                    <Button
                      size="small"
                      type="text"
                      icon={pack.enabled ? <CloseOutlined /> : <CheckOutlined />}
                      onClick={() => onTogglePackEnabled(pack)}
                    />
                  </Tooltip>
                  <Popconfirm
                    title={t('nl2sql.references.deletePackConfirm')}
                    okText={t('common.delete')}
                    cancelText={t('common.cancel')}
                    okButtonProps={{ danger: true }}
                    onConfirm={() => onDeletePack(pack)}
                  >
                    <Button
                      size="small"
                      type="text"
                      danger
                      icon={<DeleteOutlined />}
                    />
                  </Popconfirm>
                </div>
                {pack.files.length > 0 && (
                  <Checkbox.Group
                    disabled={includeAll}
                    value={activeFileIds}
                    onChange={(values) => onChangeFiles(values.map(String))}
                    style={{ display: 'grid', gap: 4, marginTop: 8, paddingLeft: 24 }}
                  >
                    {pack.files.map((file) => (
                      <div
                        key={file.id}
                        style={{
                          display: 'flex',
                          alignItems: 'center',
                          justifyContent: 'space-between',
                          gap: 6,
                          minWidth: 0,
                        }}
                      >
                        <Checkbox value={file.id} style={{ fontSize: 11, minWidth: 0, flex: 1 }} disabled={!pack.enabled}>
                          <span style={{ display: 'inline-flex', gap: 6, alignItems: 'center', maxWidth: 260, minWidth: 0 }}>
                            <FileTextOutlined style={{ color: 'var(--text-muted)', flex: '0 0 auto' }} />
                            <Text style={{ fontSize: 11, maxWidth: 170 }} ellipsis={{ tooltip: file.filename }}>
                              {file.filename}
                            </Text>
                            <Tag style={{ marginInlineEnd: 0, fontSize: 9 }}>{file.chunkCount}</Tag>
                          </span>
                        </Checkbox>
                        <Popconfirm
                          title={t('nl2sql.references.deleteFileConfirm')}
                          okText={t('common.delete')}
                          cancelText={t('common.cancel')}
                          okButtonProps={{ danger: true }}
                          onConfirm={() => onDeleteFile(file.id)}
                        >
                          <Button
                            size="small"
                            type="text"
                            danger
                            icon={<DeleteOutlined />}
                            style={{ width: 22, height: 22 }}
                          />
                        </Popconfirm>
                      </div>
                    ))}
                  </Checkbox.Group>
                )}
              </div>
            ))}
          </Checkbox.Group>
        </div>
      )}
    </div>
  );
}

function compactForSqlComment(value: string, maxChars = 1200) {
  const cleaned = value
    .replace(/\r\n/g, '\n')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .join('\n');
  return cleaned.length > maxChars ? `${cleaned.slice(0, maxChars)}...` : cleaned;
}

function sqlCommentBlock(label: string, value?: string | null) {
  const text = compactForSqlComment(value ?? '');
  if (!text) return '';
  const lines = text.split('\n').map((line) => `--   ${line}`);
  return [`-- ${label}:`, ...lines].join('\n');
}

function safeSqlKnowledgeFilename(question?: string, queryId?: string | null) {
  const slug = (question ?? 'query')
    .trim()
    .replace(/[\\/:*?"<>|#%{}]+/g, '_')
    .replace(/\s+/g, '_')
    .replace(/_+/g, '_')
    .replace(/^_+|_+$/g, '')
    .slice(0, 64) || 'query';
  const suffix = (queryId ?? '').trim().slice(-8) || 'manual';
  return `nl2sql_${slug}_${suffix}.sql`;
}

function buildSqlKnowledgeFileContent({
  question,
  sql,
  dataSourceId,
  queryId,
  explanation,
  result,
}: {
  question?: string;
  sql: string;
  dataSourceId?: string | null;
  queryId?: string | null;
  explanation?: string | null;
  result?: QueryResult;
}) {
  const columns = result?.columns?.join(', ') ?? '';
  const rowCount = result
    ? String(result.total_rows ?? result.row_count ?? result.rows?.length ?? 0)
    : '';
  return [
    '-- AOS SQL Knowledge Example',
    '-- Saved from Data Exploration after successful execution.',
    sqlCommentBlock('Question', question),
    dataSourceId ? `-- Data source ID: ${dataSourceId}` : '',
    queryId ? `-- Query ID: ${queryId}` : '',
    columns ? `-- Result columns: ${columns}` : '',
    rowCount ? `-- Result rows: ${rowCount}` : '',
    sqlCommentBlock('Explanation', explanation),
    '-- Reuse note: live schema and permissions are authoritative; adapt filters before running.',
    '',
    sql.trim(),
    '',
  ].filter(Boolean).join('\n');
}

function ValidationHitsPanel({
  score,
  warnings,
  suggestions,
  t,
}: {
  score?: number | null;
  warnings?: import('@/types').ValidationWarning[] | null;
  suggestions?: string[] | null;
  t: ReturnType<typeof useTranslation>['t'];
}) {
  const [expanded, setExpanded] = useState(false);
  const warningList = warnings ?? [];
  const suggestionList = suggestions ?? [];
  const hasContent = score != null || warningList.length > 0 || suggestionList.length > 0;
  if (!hasContent) return null;

  const errorCount = warningList.filter((w) => w.severity === 'error').length;
  const warnCount = warningList.length - errorCount;

  return (
    <div style={{ marginTop: 6 }}>
      <Button
        size="small"
        type="text"
        onClick={() => setExpanded((v) => !v)}
        style={{ paddingInline: 6, height: 22, fontSize: 11, color: 'var(--text-secondary)' }}
        icon={expanded ? <UpOutlined /> : <DownOutlined />}
      >
        {`校验规则命中详情${score != null ? ` · 评分 ${Math.round(score * 100)}%` : ''}${warningList.length ? ` · 告警 ${warningList.length}` : ''}`}
      </Button>
      {expanded && (
        <div style={{
          marginTop: 6,
          padding: '8px 10px',
          borderRadius: 8,
          border: '1px solid var(--border-subtle)',
          background: 'var(--bg-elevated)',
          display: 'grid',
          gap: 6,
        }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
            {score != null && (
              <Tag style={{ marginInlineEnd: 0, fontSize: 10 }}>评分: {Math.round(score * 100)}%</Tag>
            )}
            {errorCount > 0 && (
              <Tag color="red" style={{ marginInlineEnd: 0, fontSize: 10 }}>
                error {errorCount}
              </Tag>
            )}
            {warnCount > 0 && (
              <Tag color="orange" style={{ marginInlineEnd: 0, fontSize: 10 }}>
                warning {warnCount}
              </Tag>
            )}
            {warningList.length === 0 && (
              <Tag color="green" style={{ marginInlineEnd: 0, fontSize: 10 }}>
                无告警
              </Tag>
            )}
          </div>

          {warningList.length > 0 && (
            <div style={{ display: 'grid', gap: 4 }}>
              {warningList.map((w, idx) => (
                <div
                  key={`validation-warning-${w.table}-${w.column}-${idx}`}
                  style={{ fontSize: 11, color: 'var(--text-secondary)', lineHeight: 1.5 }}
                >
                  <Tag
                    color={w.severity === 'error' ? 'red' : 'orange'}
                    style={{ marginInlineEnd: 6, fontSize: 10 }}
                  >
                    {w.ruleType}
                  </Tag>
                  <span style={{ color: 'var(--text-primary)' }}>{w.table}{w.column ? `.${w.column}` : ''}</span>
                  <span> · {w.message}</span>
                </div>
              ))}
            </div>
          )}

          {suggestionList.length > 0 && (
            <div style={{ display: 'grid', gap: 4 }}>
              {suggestionList.map((s, idx) => (
                <div
                  key={`validation-suggestion-${idx}`}
                  style={{ fontSize: 11, color: 'var(--text-secondary)', lineHeight: 1.5 }}
                >
                  {`- ${s}`}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function MultiSourceStepsPanel({
  steps,
  totalExecutionMs,
  stageTimeline,
  t,
}: {
  steps: AgentStepResult[];
  totalExecutionMs?: number | null;
  stageTimeline?: QueryStageTimelineItem[];
  t: ReturnType<typeof useTranslation>['t'];
}) {
  const [timelineExpanded, setTimelineExpanded] = useState(false);
  const [expandedStepKeys, setExpandedStepKeys] = useState<Record<string, boolean>>({});
  if (steps.length === 0) return null;
  const previewLimit = 120;
  const fmtMs = (ms?: number | null) => {
    if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return '-';
    if (ms >= 1000) return `${(ms / 1000).toFixed(ms >= 10_000 ? 0 : 1)}s`;
    return `${Math.round(ms)}ms`;
  };
  const stageRows = (stageTimeline ?? [])
    .filter((item, idx, arr) => !!item.stage && arr.findIndex((v) => v.stage === item.stage) === idx);
  const uniqueDatasourceCount = new Set(
    steps
      .map((step) => step.datasource_id)
      .filter((id): id is string => !!id),
  ).size;
  return (
    <div style={{
      background: 'rgba(99, 102, 241, 0.04)',
      border: '1px solid rgba(99, 102, 241, 0.2)',
      borderRadius: 12,
      padding: '10px 12px',
      marginBottom: 10,
    }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
        <Tag color="purple" style={{ marginRight: 0, fontSize: 11 }}>
          {t('nl2sql.agent.title')}
        </Tag>
        <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
          {t('nl2sql.agent.totalSteps', { count: steps.length })}
          {typeof totalExecutionMs === 'number' && totalExecutionMs > 0 ? ` · ${formatDuration(totalExecutionMs)}` : ''}
        </Text>
        {uniqueDatasourceCount <= 1 && (
          <Tag color="gold" style={{ marginRight: 0, fontSize: 10 }}>
            {t('nl2sql.agent.singleSourcePlan')}
          </Tag>
        )}
      </div>
      {!!stageRows.length && (
        <div style={{ marginBottom: 10 }}>
          <Button
            type="text"
            size="small"
            onClick={() => setTimelineExpanded((prev) => !prev)}
            style={{
              padding: 0,
              height: 22,
              color: 'var(--text-secondary)',
              display: 'inline-flex',
              alignItems: 'center',
              gap: 6,
            }}
          >
            {timelineExpanded ? <DownOutlined /> : <RightOutlined />}
            <Text style={{ fontSize: 11, color: 'var(--text-secondary)' }}>
              {t('nl2sql.routeStageTimingDetails', { count: stageRows.length })}
            </Text>
          </Button>
          {timelineExpanded && (
            <div style={{ display: 'grid', gap: 4, marginTop: 6 }}>
              {stageRows.map((item, idx) => (
                <div
                  key={`${item.stage}-${idx}`}
                  style={{
                    display: 'grid',
                    gridTemplateColumns: '160px 72px 1fr',
                    gap: 8,
                    alignItems: 'center',
                    fontSize: 11,
                    color: 'var(--text-secondary)',
                    minHeight: 18,
                  }}
                >
                  <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                    {item.label ?? getRouteStageText(t, item.stage)}
                  </Text>
                  <Text style={{ fontSize: 11, color: 'var(--text-secondary)', textAlign: 'right' }}>
                    {fmtMs(item.stageElapsedMs)}
                  </Text>
                  <Text style={{ fontSize: 11, color: 'var(--text-muted)' }} ellipsis>
                    {resolveRouteStageMessage(t, item.stage, item.message)}
                  </Text>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
      <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
        {steps.map((step, i) => {
          const stepKey = `step-${step.step_id}-${i}-${step.datasource_id ?? 'na'}`;
          const expanded = !!expandedStepKeys[stepKey];
          // Older persisted agent results and failed merge steps may omit
          // either array. Normalize at the rendering boundary so one partial
          // step cannot take down the entire Data Exploration page.
          const normalizedRows = Array.isArray(step.rows) ? step.rows : [];
          const stepColumns = Array.isArray(step.columns) ? step.columns : [];
          const visibleRows = expanded ? normalizedRows : normalizedRows.slice(0, previewLimit);
          const stepRows = visibleRows.map((row, rowIdx) => ({
            ...row,
            __rowKey: `step-${step.step_id}-${i}-row-${rowIdx}`,
          }));
          return (
          <Card
            key={stepKey}
            size="small"
            style={{
              border: step.error
                ? '1px solid rgba(239, 68, 68, 0.4)'
                : '1px solid var(--border-subtle)',
              borderRadius: 10,
            }}
            styles={{ body: { padding: '12px 14px' } }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
              <Tag color={step.error ? 'red' : step.step_type === 'merge' ? 'blue' : 'green'} style={{ fontSize: 10, marginRight: 0 }}>
                {step.step_type === 'error' ? t('nl2sql.error') : step.step_type === 'merge' ? t('nl2sql.agent.mergeResult') : t('nl2sql.agent.step', { n: i + 1 })}
              </Tag>
              {step.datasource_id && (
                <Tag style={{ fontSize: 10 }}>
                  <DatabaseOutlined style={{ marginRight: 3 }} />
                  {step.datasource_id}
                </Tag>
              )}
              <Text style={{ fontSize: 11, color: 'var(--text-muted)' }} ellipsis>
                {step.description || (step.step_type === 'merge' ? t('nl2sql.agent.mergeResult') : t('nl2sql.agent.step', { n: i + 1 }))}
              </Text>
              <Text style={{ fontSize: 11, color: 'var(--text-muted)', marginLeft: 'auto' }}>
                {formatDuration(step.execution_ms)}
                {step.row_count > 0 && ` · ${step.row_count} ${t('nl2sql.rows')}`}
              </Text>
              {normalizedRows.length > previewLimit && (
                <Button
                  size="small"
                  type="text"
                  onClick={() => setExpandedStepKeys((prev) => ({ ...prev, [stepKey]: !expanded }))}
                >
                  {expanded ? t('nl2sql.collapse') : t('nl2sql.expand')}
                </Button>
              )}
            </div>
            {step.error ? (
              <div style={{ display: 'grid', gap: 6 }}>
                <Text type="danger" style={{ fontSize: 12 }}>{step.error}</Text>
                {step.sql && (
                  <pre style={{
                    margin: 0,
                    padding: 10,
                    borderRadius: 8,
                    background: 'var(--bg-void)',
                    border: '1px solid var(--border-subtle)',
                    fontSize: 11,
                    whiteSpace: 'pre-wrap',
                    wordBreak: 'break-word',
                  }}>
                    {step.sql}
                  </pre>
                )}
              </div>
            ) : (
              <>
                {step.sql && (
                  <pre style={{
                    margin: '0 0 8px 0',
                    padding: 10,
                    borderRadius: 8,
                    background: 'var(--bg-void)',
                    border: '1px solid var(--border-subtle)',
                    fontSize: 11,
                    whiteSpace: 'pre-wrap',
                    wordBreak: 'break-word',
                  }}>
                    {step.sql}
                  </pre>
                )}
                {stepColumns.length > 0 && (
                  <div style={{ overflowX: 'auto' }}>
                    <Table
                      dataSource={stepRows}
                      columns={stepColumns.map((col, colIdx) => ({
                        title: col,
                        dataIndex: col,
                        key: `${col}-${colIdx}`,
                        width: 140,
                        ellipsis: true,
                      }))}
                      rowKey="__rowKey"
                      size="small"
                      pagination={false}
                      scroll={{ x: 'max-content', y: 160 }}
                      tableLayout="fixed"
                      style={{ fontSize: 11 }}
                    />
                    {!expanded && normalizedRows.length > previewLimit && (
                      <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                        {t('nl2sql.agent.previewRows', { preview: previewLimit, total: normalizedRows.length })}
                      </Text>
                    )}
                  </div>
                )}
              </>
            )}
          </Card>
          );
        })}
      </div>
    </div>
  );
}

const CONVERSATION_PAGE_SIZE = 20;
const DEFAULT_TABLE_PAGE_SIZE = 10;
const DEFAULT_AUTO_EXECUTE_PAGE_SIZE = 100;

// ─── SQL Card ──────────────────────────────────────────────────────────────────

function SqlCard({
  sql,
  explanation,
  isEditable,
  editedSql,
  onEdit,
  onConfirm,
  onCancel,
  isGenerating,
  stage,
  stageMessage,
  elapsedMs,
  stageTimeline,
  t,
}: {
  sql: string;
  explanation?: string;
  isEditable?: boolean;
  editedSql?: string;
  onEdit?: (sql: string) => void;
  onConfirm?: () => void;
  onCancel?: () => void;
  isGenerating?: boolean;
  stage?: string | null;
  stageMessage?: string | null;
  elapsedMs?: number | null;
  stageTimeline?: QueryStageTimelineItem[];
  t: ReturnType<typeof useTranslation>['t'];
}) {
  const [copied, setCopied] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(sql);
  const [stageTimingExpanded, setStageTimingExpanded] = useState(false);

  const displaySql = editing ? draft : sql;

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(displaySql);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch { /* ignore */ }
  };

  const handleEditSave = () => {
    onEdit?.(draft);
    setEditing(false);
  };

  const handleEditCancel = () => {
    setDraft(sql);
    setEditing(false);
    onCancel?.();
  };

  if (isGenerating) {
    const stageLabel = stage ? getRouteStageText(t, stage) : t('nl2sql.generating');
    const stageMessageText = resolveRouteStageMessage(t, stage, stageMessage);
    const extra = stageMessageText && stageMessageText !== stageLabel ? ` · ${stageMessageText}` : '';
    const timeText = typeof elapsedMs === 'number' && elapsedMs > 0
      ? ` · ${(elapsedMs / 1000).toFixed(elapsedMs >= 10_000 ? 0 : 1)}s`
      : '';
    const timelineStages = (stageTimeline ?? [])
      .filter((item) => item.kind !== 'step')
      .map((item) => item.stage)
      .filter((s): s is string => !!s)
      .filter((s, idx, arr) => arr.indexOf(s) === idx);
    const visibleStages = timelineStages.length > 0
      ? [...timelineStages]
      : (stage ? [stage] : []);
    if (stage && !visibleStages.includes(stage)) {
      visibleStages.push(stage);
    }
    const currentIndex = stage ? visibleStages.indexOf(stage) : -1;
    const stageRows = (stageTimeline ?? [])
      .filter((item, idx, arr) => !!item.stage && arr.findIndex((v) => v.stage === item.stage) === idx);
    const fmtMs = (ms?: number | null) => {
      if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return '-';
      if (ms >= 1000) return `${(ms / 1000).toFixed(ms >= 10_000 ? 0 : 1)}s`;
      return `${Math.round(ms)}ms`;
    };
    return (
      <div style={{
        background: 'var(--bg-void)',
        border: '1px solid var(--border-default)',
        borderRadius: 10,
        padding: '16px 20px',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
          <LoadingOutlined spin style={{ color: 'var(--accent-ai)' }} />
          <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
            {stageLabel}{extra}{timeText}
          </Text>
        </div>
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginBottom: 8 }}>
          {visibleStages.map((step, idx) => {
            const done = currentIndex > idx;
            const running = currentIndex === idx;
            return (
              <span
                key={`${step}-${idx}`}
                style={{
                  fontSize: 10,
                  lineHeight: '16px',
                  borderRadius: 999,
                  padding: '0 6px',
                  border: running
                    ? '1px solid rgba(124,58,237,0.65)'
                    : done
                      ? '1px solid rgba(16,185,129,0.55)'
                      : '1px solid rgba(148,163,184,0.35)',
                  background: running
                    ? 'rgba(124,58,237,0.18)'
                    : done
                      ? 'rgba(16,185,129,0.16)'
                      : 'rgba(148,163,184,0.08)',
                  color: running
                    ? '#c4b5fd'
                    : done
                      ? '#86efac'
                      : 'var(--text-muted)',
                }}
              >
                {done ? '✓ ' : running ? '• ' : ''}{getRouteStageText(t, step)}
              </span>
            );
          })}
        </div>
        <div style={{
          height: 4, borderRadius: 2, background: 'var(--bg-interactive)',
          overflow: 'hidden',
        }}>
          <div style={{
            height: '100%', width: '60%', borderRadius: 2,
            background: 'linear-gradient(90deg, var(--accent-ai), rgba(124,58,237,0.6))',
            animation: 'shimmer 1.5s infinite',
          }} />
        </div>
        {!!stageRows.length && (
          <div style={{ marginTop: 10 }}>
            <Button
              type="text"
              size="small"
              onClick={() => setStageTimingExpanded((prev) => !prev)}
              style={{
                padding: 0,
                height: 22,
                color: 'var(--text-secondary)',
                display: 'inline-flex',
                alignItems: 'center',
                gap: 6,
              }}
            >
              {stageTimingExpanded ? <DownOutlined /> : <RightOutlined />}
              <Text style={{ fontSize: 11, color: 'var(--text-secondary)' }}>
                {t('nl2sql.routeStageTimingDetails', { count: stageRows.length })}
              </Text>
            </Button>
            {stageTimingExpanded && (
              <div style={{ display: 'grid', gap: 4, marginTop: 6 }}>
                {stageRows.map((item, idx) => (
                  <div
                    key={`${item.stage}-${idx}`}
                    style={{
                      display: 'grid',
                      gridTemplateColumns: '120px 72px 1fr',
                      gap: 8,
                      alignItems: 'center',
                      fontSize: 11,
                      color: 'var(--text-secondary)',
                      minHeight: 18,
                    }}
                  >
                    <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                      {item.label ?? getRouteStageText(t, item.stage)}
                    </Text>
                    <Text style={{ fontSize: 11, color: 'var(--text-secondary)', textAlign: 'right' }}>
                      {fmtMs(item.stageElapsedMs)}
                    </Text>
                    <Text style={{ fontSize: 11, color: 'var(--text-muted)' }} ellipsis>
                      {resolveRouteStageMessage(t, item.stage, item.message)}
                    </Text>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    );
  }

  return (
    <div style={{
      background: 'var(--bg-void)',
      border: '1px solid var(--border-default)',
      borderRadius: 10,
      overflow: 'hidden',
    }}>
      <div style={{
        display: 'flex', alignItems: 'center', gap: 8,
        padding: '6px 12px',
        background: 'rgba(24,144,255,0.08)',
        borderBottom: '1px solid rgba(24,144,255,0.15)',
      }}>
        <span style={{ fontSize: 12, color: 'var(--text-secondary)', fontWeight: 600 }}>{t('nl2sql.sqlLabel')}</span>
        <Tag color="blue" style={{ fontSize: 10 }}>{t('nl2sql.generated')}</Tag>
        <div style={{ marginLeft: 'auto', display: 'flex', gap: 4 }}>
          <Tooltip title={t('nl2sql.copySql')}>
            <Button
              type="text" size="small"
              icon={copied ? <CheckOutlined /> : <CopyOutlined />}
              onClick={handleCopy}
              style={{ color: 'var(--text-muted)', padding: '2px 6px', height: 22, fontSize: 11 }}
            />
          </Tooltip>
          {isEditable && !editing && (
            <Tooltip title={t('nl2sql.editSql')}>
              <Button
                type="text" size="small"
                icon={<EditOutlined />}
                onClick={() => { setDraft(displaySql); setEditing(true); }}
                style={{ color: 'var(--text-muted)', padding: '2px 6px', height: 22, fontSize: 11 }}
              />
            </Tooltip>
          )}
        </div>
      </div>

      {explanation && (
        <div style={{
          padding: '8px 12px',
          borderBottom: '1px solid var(--border-default)',
          fontSize: 13, color: 'var(--text-secondary)',
          lineHeight: 1.6,
        }}>
          <Markdown relaxed>{explanation}</Markdown>
        </div>
      )}

      {editing ? (
        <div>
          <Editor
            height={Math.min(Math.max(draft.split('\n').length * 19 + 24, 60), 320)}
            language="sql"
            value={draft}
            theme="vs-dark"
            onChange={(val) => setDraft(val ?? '')}
            options={{
              minimap: { enabled: false },
              scrollBeyondLastLine: false,
              lineNumbers: 'on',
              glyphMargin: false,
              folding: true,
              wordWrap: 'on',
              scrollbar: { vertical: 'auto', horizontal: 'auto' },
              fontSize: 13,
              fontFamily: "'JetBrains Mono', 'Fira Code', monospace",
              padding: { top: 8, bottom: 8 },
              tabSize: 2,
            }}
          />
          <div style={{ display: 'flex', gap: 8, padding: '6px 12px', borderTop: '1px solid var(--border-default)', justifyContent: 'flex-end' }}>
            <Button size="small" onClick={handleEditCancel}>
              {t('nl2sql.cancel')}
            </Button>
            <Button size="small" type="primary" onClick={handleEditSave} icon={<CheckOutlined />}>
              {t('nl2sql.saveChanges')}
            </Button>
          </div>
        </div>
      ) : (
        <Editor
          height={Math.min(Math.max(displaySql.split('\n').length * 19 + 24, 60), 280)}
          language="sql"
          value={displaySql}
          theme="vs-dark"
          options={{
            readOnly: true,
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
            lineNumbers: 'off',
            glyphMargin: false,
            folding: false,
            lineDecorationsWidth: 0,
            lineNumbersMinChars: 0,
            wordWrap: 'on',
            overviewRulerLanes: 0,
            hideCursorInOverviewRuler: true,
            overviewRulerBorder: false,
            scrollbar: { vertical: 'auto', horizontal: 'hidden' },
            renderLineHighlight: 'none',
            fontSize: 13,
            fontFamily: "'JetBrains Mono', 'Fira Code', monospace",
            padding: { top: 8, bottom: 8 },
          }}
        />
      )}
    </div>
  );
}

// ─── Result Table ───────────────────────────────────────────────────────────────

function ResultTable({
  result,
  onDownloadCSV,
  onDownloadExcel,
  onDownloadJSON,
  shareUrl,
  t,
}: {
  result: QueryResult;
  onDownloadCSV?: () => void;
  onDownloadExcel?: () => void;
  onDownloadJSON?: () => void;
  shareUrl?: string;
  t: ReturnType<typeof useTranslation>['t'];
}) {
  if (result.rows.length === 0) {
    return (
      <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-muted)' }}>
        <span style={{ fontSize: 32, display: 'block', marginBottom: 8 }}>📭</span>
        <Text style={{ fontSize: 13 }}>{t('nl2sql.noResults')}</Text>
      </div>
    );
  }

  const columns = result.columns.map((col) => ({
    title: (
      <span style={{ fontSize: 12, fontWeight: 600 }}>
        {col}
      </span>
    ),
    dataIndex: col,
    key: col,
    width: 160,
    ellipsis: true,
    render: (val: unknown) => {
      if (val === null || val === undefined) {
        return <span style={{ color: 'var(--text-muted)', fontStyle: 'italic', fontSize: 12 }}>{t('nl2sql.nullValue')}</span>;
      }
      const str = String(val);
      if (str.length > 80) {
        return (
          <Tooltip title={str}>
            <span>{str.slice(0, 80)}…</span>
          </Tooltip>
        );
      }
      return <span style={{ fontSize: 13 }}>{str}</span>;
    },
  }));

  // F-10: Sample up to 20 rows and verify ALL non-null values are numeric.
  const SAMPLE_SIZE = 20;
  const sampleRows = result.rows.slice(0, SAMPLE_SIZE);
  const numericCols = result.columns.filter((col) =>
    sampleRows.every((row) => {
      const val = row[col];
      return val === null || val === undefined || typeof val === 'number';
    })
  );

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '4px 0' }}>
        <TableOutlined style={{ color: 'var(--accent-ai)' }} />
        <Text style={{ fontSize: 12, color: 'var(--text-primary)', fontWeight: 600 }}>
          {result.total_rows != null && result.total_rows > result.row_count
            ? t('nl2sql.resultCountOf', { count: result.row_count, total: result.total_rows.toLocaleString() })
            : t('nl2sql.resultCount', { count: result.row_count })}
        </Text>
        {result.execution_time_ms != null && (
          <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
            · {formatDuration(result.execution_time_ms)}
          </Text>
        )}
        {numericCols.length > 0 && (
          <Tag color="purple" style={{ fontSize: 10 }}>
            {numericCols.length} {t('nl2sql.numericCols')}
          </Tag>
        )}
        <div style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
          <Dropdown menu={{
          items: [
            { key: 'csv', label: t('nl2sql.exportCsv'), onClick: onDownloadCSV },
            { key: 'xlsx', label: t('nl2sql.exportExcel'), onClick: onDownloadExcel },
            { key: 'json', label: t('nl2sql.exportJson'), onClick: onDownloadJSON },
            ...(shareUrl ? [
              { type: 'divider' as const },
              { key: 'share', label: t('nl2sql.copyShareLink'), icon: <ShareAltOutlined />, onClick: async () => {
                try {
                  await navigator.clipboard.writeText(shareUrl);
                  message.success(t('nl2sql.shareLinkCopied'));
                } catch { /* ignore */ }
              }},
            ] : []),
          ],
        }} trigger={['click']}>
          <Button size="small" icon={<DownloadOutlined />} style={{ fontSize: 11 }}>
            {t('nl2sql.exportTitle')} <DownOutlined />
          </Button>
        </Dropdown>
        </div>
      </div>

      <div style={{ border: '1px solid var(--border-default)', borderRadius: 8, overflow: 'hidden' }}>
        <Table
          columns={columns}
          dataSource={result.rows}
          rowKey={(_, i) => String(i)}
          size="small"
          scroll={{ x: 'max-content', y: 400 }}
          pagination={false}
          style={{ fontSize: 13 }}
          summary={() => {
            if (numericCols.length === 0) return null;
            return (
              <Table.Summary fixed>
                <Table.Summary.Row>
                  {result.columns.map((col, i) => (
                    <Table.Summary.Cell key={col} index={i}>
                      {numericCols.includes(col) ? (
                        <Text style={{ fontSize: 11, color: 'var(--text-muted)', fontStyle: 'italic' }}>
                          {t('nl2sql.sum')} {result.rows.reduce((s, r) => s + (Number(r[col]) || 0), 0).toLocaleString()}
                        </Text>
                      ) : null}
                    </Table.Summary.Cell>
                  ))}
                </Table.Summary.Row>
              </Table.Summary>
            );
          }}
        />
      </div>
    </div>
  );
}

// ─── Execute Button ─────────────────────────────────────────────────────────────

function ExecuteBar({
  sql,
  canExecute = true,
  canSaveView = true,
  isExecuting,
  hasResult,
  isEditing,
  isSavingKnowledge,
  canSaveKnowledge = true,
  onExecute,
  onSaveView,
  onSaveKnowledge,
  t,
}: {
  sql: string;
  canExecute?: boolean;
  canSaveView?: boolean;
  canSaveKnowledge?: boolean;
  isExecuting?: boolean;
  isSavingKnowledge?: boolean;
  hasResult?: boolean;
  isEditing?: boolean;
  onExecute?: () => void;
  onSaveView?: () => void;
  onSaveKnowledge?: () => void;
  t: ReturnType<typeof useTranslation>['t'];
}) {
  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 8,
      padding: '8px 0',
      borderTop: '1px solid var(--border-subtle)',
      marginTop: 4,
    }}>
      <Tooltip title={canExecute ? undefined : t('nl2sql.missingQueryId')}>
        <Button
          type="primary"
          icon={isExecuting ? <LoadingOutlined spin /> : <PlayCircleOutlined />}
          onClick={onExecute}
          loading={isExecuting}
          disabled={!sql || !canExecute}
          style={{ borderRadius: 8 }}
        >
          {isExecuting
            ? t('nl2sql.executing')
            : hasResult
              ? t('nl2sql.reExecute')
              : t('nl2sql.executeSql')}
        </Button>
      </Tooltip>
      {sql && (
        <Tooltip title={canSaveView ? undefined : t('nl2sql.missingQueryId')}>
          <Button
            icon={<SaveOutlined />}
            onClick={onSaveView}
            disabled={!canSaveView}
            style={{ borderRadius: 8 }}
          >
            {t('nl2sql.saveAsView')}
          </Button>
        </Tooltip>
      )}
      {sql && hasResult && (
        <Tooltip title={canSaveKnowledge ? t('nl2sql.saveToKnowledgeHint') : t('nl2sql.embeddingRequiredForKnowledgeSave')}>
          <Button
            icon={isSavingKnowledge ? <LoadingOutlined spin /> : <FileTextOutlined />}
            onClick={onSaveKnowledge}
            loading={isSavingKnowledge}
            disabled={!canSaveKnowledge || isSavingKnowledge}
            style={{ borderRadius: 8 }}
          >
            {isSavingKnowledge ? t('nl2sql.savingToKnowledge') : t('nl2sql.saveToKnowledge')}
          </Button>
        </Tooltip>
      )}
      <Text style={{ fontSize: 11, color: 'var(--text-muted)', marginLeft: 'auto' }}>
        <InfoCircleOutlined style={{ marginRight: 4 }} />
        {hasResult ? t('nl2sql.autoExecutedReviewHint') : t('nl2sql.reviewBeforeExecute')}
      </Text>
    </div>
  );
}

// ─── Conversation Drawer ─────────────────────────────────────────────────────────

function ConversationDrawer({
  open,
  onClose,
  onSelectConversation,
  t,
}: {
  open: boolean;
  onClose: () => void;
  onSelectConversation: (id: string, summary: string | null, messageCount: number) => void;
  t: ReturnType<typeof useTranslation>['t'];
}) {
  const { data, isLoading } = useQuery({
    queryKey: queryKeys.nl2sql.conversations.list(),
    queryFn: () => nl2sqlApi.listConversations(),
    enabled: open,
    staleTime: 30_000,
  });

  return (
    <Drawer
      title={
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <CommentOutlined style={{ color: 'var(--accent-ai)' }} />
          <span>{t('nl2sql.conversations')}</span>
        </div>
      }
      placement="right"
      onClose={onClose}
      open={open}
      width={520}
    >
      {isLoading ? (
        <div style={{ padding: 24, textAlign: 'center' }}>
          <Spin />
        </div>
      ) : !data?.conversations || data.conversations.length === 0 ? (
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('nl2sql.noConversations')} />
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
          {data.conversations.map((conv) => (
            <Card
              key={conv.id}
              size="small"
              hoverable
              onClick={() => onSelectConversation(conv.id, conv.summary, conv.message_count)}
              style={{ cursor: 'pointer' }}
              title={
                <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                  <MessageOutlined style={{ color: 'var(--accent-ai)', fontSize: 11 }} />
                  <span style={{
                    fontSize: 12, flex: 1, overflow: 'hidden',
                    textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                  }}>
                    {conv.last_question ?? t('nl2sql.untitledConversation')}
                  </span>
                </div>
              }
              extra={
                <Space size={4}>
                  <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                    {formatTime(conv.updated_at)}
                  </Text>
                </Space>
              }
            >
              {conv.summary ? (
                <div>
                  <Text type="secondary" style={{ fontSize: 11 }}>
                    <FileTextOutlined style={{ marginRight: 4 }} />
                    {t('nl2sql.summary')}:
                  </Text>
                  <div style={{
                    fontSize: 11, color: 'var(--text-secondary)',
                    marginTop: 4, lineHeight: 1.5,
                    display: '-webkit-box', WebkitLineClamp: 3, WebkitBoxOrient: 'vertical' as const,
                    overflow: 'hidden',
                  }}>
                    {conv.summary}
                  </div>
                </div>
              ) : (
                <Text type="secondary" style={{ fontSize: 11 }}>
                  {conv.message_count} {t('nl2sql.queries')}
                </Text>
              )}
              <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginTop: 6 }}>
                <Tag color="blue" style={{ fontSize: 10 }}>
                  {conv.message_count} {t('nl2sql.messages')}
                </Tag>
                {conv.summary && (
                  <Tag color="green" style={{ fontSize: 10 }}>
                    {t('nl2sql.hasSummary')}
                  </Tag>
                )}
              </div>
            </Card>
          ))}
        </div>
      )}
    </Drawer>
  );
}

// ─── Data Source Picker ────────────────────────────────────────────────────────

function DataSourcePicker({
  dataSources,
  selectedId,
  onSelect,
  t,
}: {
  dataSources: DataSourceInfo[];
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  t: ReturnType<typeof useTranslation>['t'];
}) {
  if (dataSources.length === 0) {
    return (
      <div style={{ padding: '16px', textAlign: 'center', color: 'var(--text-muted)', fontSize: 13 }}>
        {t('nl2sql.noDataSources')}
      </div>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      {dataSources.map((ds) => {
        const isSelected = ds.id === selectedId;
        return (
          <div
            key={ds.id}
            onClick={() => onSelect(isSelected ? null : ds.id)}
            style={{
              padding: '10px 12px',
              borderRadius: 8,
              cursor: 'pointer',
              background: isSelected ? 'rgba(124,58,237,0.08)' : 'var(--bg-elevated)',
              border: `1px solid ${isSelected ? 'var(--accent-ai)' : 'var(--border-default)'}`,
              transition: 'all 0.15s',
              display: 'flex',
              gap: 10,
              alignItems: 'flex-start',
              position: 'relative',
            }}
          >
            {isSelected && (
              <div style={{
                position: 'absolute', top: -1, left: -1,
                background: 'var(--accent-ai)', color: '#fff',
                fontSize: 9, padding: '1px 5px', borderRadius: '4px 0 4px 0',
                fontWeight: 600, lineHeight: '14px',
              }}>已选</div>
            )}
            <div style={{
              width: 18, height: 18, borderRadius: 4,
              border: `2px solid ${isSelected ? 'var(--accent-ai)' : 'var(--border-default)'}`,
              background: isSelected ? 'var(--accent-ai)' : 'transparent',
              display: 'flex', alignItems: 'center', justifyContent: 'center',
              flexShrink: 0, marginTop: 2, transition: 'all 0.15s',
            }}>
              {isSelected && <span style={{ color: '#fff', fontSize: 11, lineHeight: 1 }}>✓</span>}
            </div>
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                <DatabaseOutlined style={{ fontSize: 12, color: 'var(--text-secondary)' }} />
                <Text style={{ fontSize: 13, fontWeight: 600, color: 'var(--text-primary)' }} ellipsis>
                  {ds.name}
                </Text>
                <Tag style={{ fontSize: 9 }}>{t(`nl2sql.dbType.${ds.db_type}` as const, { defaultValue: ds.db_type })}</Tag>
              </div>
              {ds.description && (
                <Text type="secondary" style={{ fontSize: 11, display: 'block', marginTop: 2 }} ellipsis>
                  {ds.description}
                </Text>
              )}
              {dataSourceTables(ds).length > 0 && (
                <Text style={{ fontSize: 10, color: 'var(--text-muted)', marginTop: 2 }}>
                  {dataSourceTables(ds).length} {t('datasources.tables')}
                </Text>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}

// ─── Saved Views Drawer ───────────────────────────────────────────────────────

// ─── Inline-rename wrapper for saved-view cards ──────────────────────────────────

function SavedViewCard({
  view,
  onLoad,
  onDelete,
  onRename,
  t,
}: {
  view: EditableView;
  onLoad: (v: EditableView) => void;
  onDelete?: (id: string) => void;
  onRename: (id: string, data: { name?: string; description?: string }) => void;
  t: (key: string) => string;
}) {
  const [editing, setEditing] = useState(false);
  const [draftName, setDraftName] = useState(view.name);
  const [draftDesc, setDraftDesc] = useState(view.description ?? '');
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const commit = () => {
    if (draftName.trim() && draftName !== view.name) {
      setSaving(true);
      onRename(view.id, { name: draftName.trim(), description: draftDesc || undefined });
    }
    setEditing(false);
  };

  const handleDelete = async (e?: ReactMouseEvent<HTMLElement>) => {
    e?.stopPropagation();
    if (!onDelete) return;
    setDeleting(true);
    try {
      await onDelete(view.id);
    } finally {
      setDeleting(false);
    }
  };

  if (editing) {
    return (
      <Card
        size="small"
        style={{ border: '1px solid #1677ff' }}
        bodyStyle={{ padding: 12 }}
      >
        <Space direction="vertical" style={{ width: '100%' }} size={8}>
          <Input
            value={draftName}
            onChange={e => setDraftName(e.target.value)}
            onPressEnter={commit}
            placeholder={t('nl2sql.viewNamePlaceholder')}
            autoFocus
          />
          <TextArea
            value={draftDesc}
            onChange={e => setDraftDesc(e.target.value)}
            placeholder={t('nl2sql.viewDescPlaceholder')}
            rows={2}
            style={{ fontSize: 12 }}
          />
          <Space>
            <Button size="small" type="primary" loading={saving} onClick={commit}>
              {t('common.save')}
            </Button>
            <Button size="small" onClick={() => { setEditing(false); setDraftName(view.name); setDraftDesc(view.description ?? ''); }}>
              {t('common.cancel')}
            </Button>
          </Space>
        </Space>
      </Card>
    );
  }

  return (
    <Card
      size="small"
      hoverable
      onClick={() => onLoad(view)}
      style={{ cursor: 'pointer' }}
      title={
        <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
          <StarOutlined style={{ color: '#faad14', fontSize: 12 }} />
          <span style={{ fontSize: 13, fontWeight: 600, flex: 1 }}>{view.name}</span>
          <Button
            type="text" size="small" icon={<EditOutlined />}
            onClick={e => { e.stopPropagation(); setEditing(true); }}
            title={t('common.edit')}
          />
          <Popconfirm
            title={t('common.deleteConfirm')}
            onConfirm={handleDelete}
            okText={t('common.confirm')}
            cancelText={t('common.cancel')}
          >
            <Button
              type="text" size="small" danger icon={<DeleteOutlined />}
              loading={deleting}
              onClick={(e) => e.stopPropagation()}
              title={t('common.delete')}
            />
          </Popconfirm>
        </div>
      }
    >
      <Text style={{ fontSize: 12, color: 'var(--text-secondary)', display: 'block', marginBottom: 6 }}>
        {view.description || view.question}
      </Text>
      <pre style={{
        margin: 0, fontSize: 11, color: 'var(--text-secondary)',
        whiteSpace: 'pre-wrap', wordBreak: 'break-word',
        fontFamily: 'var(--font-code)', maxHeight: 50, overflow: 'hidden',
      }}>
        {view.sql}
      </pre>
    </Card>
  );
}

// ─── Saved Views Drawer ─────────────────────────────────────────────────────────

function SavedViewsDrawer({
  open,
  onClose,
  views,
  onLoad,
  onDelete,
  onRename,
  t,
}: {
  open: boolean;
  onClose: () => void;
  views: EditableView[];
  onLoad: (view: EditableView) => void;
  onDelete?: (id: string) => Promise<void> | void;
  onRename: (id: string, data: { name?: string; description?: string }) => void;
  t: ReturnType<typeof useTranslation>['t'];
}) {
  return (
    <Drawer
      title={
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <StarOutlined style={{ color: '#faad14' }} />
          <span>{t('nl2sql.savedViews')}</span>
        </div>
      }
      placement="right"
      onClose={onClose}
      open={open}
      width={400}
    >
      {views.length === 0 ? (
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('nl2sql.noSavedViews')} />
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          {views.map((view) => (
            <SavedViewCard
              key={view.id}
              view={view}
              onLoad={onLoad}
              onDelete={onDelete}
              onRename={onRename}
              t={t}
            />
          ))}
        </div>
      )}
    </Drawer>
  );
}

// ─── Quick Templates ────────────────────────────────────────────────────────────

const NL_TEMPLATES: Array<{ key: string; question: string; icon: string }> = [
  { key: 'today_total', question: '', icon: '📦' },
  { key: 'top_customers', question: '', icon: '👑' },
  { key: 'revenue_trend', question: '', icon: '📈' },
  { key: 'user_growth', question: '', icon: '👥' },
];

function useTemplateQuestions() {
  const { t } = useTranslation();
  return NL_TEMPLATES.map(tmpl => ({
    ...tmpl,
    question: t(`nl2sql.templates.${tmpl.key}`),
  }));
}

type SlashCommandKey = 'multi';

// ─── Main Component ─────────────────────────────────────────────────────────────

export default function Nl2sql() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const localEmbeddingNotice = useDismissibleNotice('aos.embedding-local-notice.v1');

  // ── Data source selection
  const [selectedDataSourceId, setSelectedDataSourceId] = useState<string | null>(null);

  // ── Conversation turns (multi-turn context)
  const [turns, setTurns] = useState<NlTurn[]>([]);
  const [input, setInput] = useState('');
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [sqlEditingId, setSqlEditingId] = useState<string | null>(null);

  // ── Drawers
  const [savedViewsDrawerOpen, setSavedViewsDrawerOpen] = useState(false);
  const [conversationDrawerOpen, setConversationDrawerOpen] = useState(false);
  const [turnViewTabs, setTurnViewTabs] = useState<Record<string, ViewTab>>({});
  const [chartType, setChartType] = useState<ChartType>('line');

  // ── Conversation ID for multi-turn
  const [conversationId, setConversationId] = useState<string | null>(null);

  // ── Quick templates (must be called unconditionally at top level — Rules of Hooks)
  const templateQuestions = useTemplateQuestions();

  // ── P3-1: Clarification state
  const [clarificationContext, setClarificationContext] = useState<ClarificationContextType | null>(null);
  const [clarifyingTurnId, setClarifyingTurnId] = useState<string | null>(null);
  const [clarifyLoading, setClarifyLoading] = useState(false);
  const [clarificationPausedByUser, setClarificationPausedByUser] = useState(false);
  const clarifyTaskUnsubMapRef = useRef<Record<string, () => void>>({});

  const resolveClarifyingTurnId = useCallback((
    list: NlTurn[],
    ctx: ClarificationContextType | null,
    preferredId: string | null,
  ): string | null => {
    if (!ctx) return null;
    if (preferredId) {
      const preferredTurn = list.find((turn) => turn.id === preferredId);
      // Guard against stale/misaligned IDs (e.g. accidentally pointing to a user turn).
      if (preferredTurn && preferredTurn.role === 'assistant') {
        return preferredId;
      }
    }
    const byQuestion = [...list].reverse().find((turn) =>
      turn.role === 'assistant'
      && !!turn.clarificationQuestion
      && turn.question === ctx.original_question
    );
    if (byQuestion?.id) return byQuestion.id;
    const latestClarify = [...list].reverse().find((turn) =>
      turn.role === 'assistant' && !!turn.clarificationQuestion
    );
    return latestClarify?.id ?? null;
  }, []);

  const effectiveClarifyingTurnId = useMemo(
    () => resolveClarifyingTurnId(turns, clarificationContext, clarifyingTurnId),
    [turns, clarificationContext, clarifyingTurnId, resolveClarifyingTurnId],
  );

  const createMultiSourceProgressTimeline = useCallback((elapsedMs: number = 0): NlTurn['queryStageTimeline'] => ([
    {
      stage: 'request_validation',
      message: t('nl2sql.routeMessages.requestValidationStart'),
      atElapsedMs: elapsedMs,
      stageElapsedMs: elapsedMs,
    },
  ]), [t]);

  // Self-heal: if the raw id drifts (e.g. rerender / list refresh), re-anchor the clarification input.
  useEffect(() => {
    if (!clarificationContext) return;
    if (!effectiveClarifyingTurnId) return;
    if (clarifyingTurnId !== effectiveClarifyingTurnId) {
      setClarifyingTurnId(effectiveClarifyingTurnId);
    }
  }, [clarificationContext, clarifyingTurnId, effectiveClarifyingTurnId]);

  // ── Table pagination state (per turn)
  const [turnTablePager, setTurnTablePager] = useState<Record<string, { page: number; pageSize: number }>>({});
  const getTurnPager = useCallback(
    (turnId: string) => turnTablePager[turnId] ?? { page: 1, pageSize: DEFAULT_TABLE_PAGE_SIZE },
    [turnTablePager],
  );
  const setTurnPager = useCallback((turnId: string, page: number, pageSize: number) => {
    setTurnTablePager((prev) => ({
      ...prev,
      [turnId]: { page, pageSize },
    }));
  }, []);
  // ── Conversation detail upward pagination (newest at bottom, scroll up for older)
  const [conversationPage, setConversationPage] = useState(1);
  const [conversationHasMore, setConversationHasMore] = useState(false);
  const [conversationLoadingMore, setConversationLoadingMore] = useState(false);
  const [activeConversationId, setActiveConversationId] = useState<string | null>(null);
  const [pendingViewConversationId, setPendingViewConversationId] = useState<string | null>(null);
  // ── F-02: Track component mount to avoid setState after unmount
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);
  // ── Session ID for clarification persistence
  const [sessionId] = useState(() => `nl2sql-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`);

  // ── AI smart source suggestion state
  const [isSuggestingSource, setIsSuggestingSource] = useState(false);
  const [routingStage, setRoutingStage] = useState<'idle' | 'search_candidates' | 'ai_confirming' | 'manual_continue' | 'ready'>('idle');
  const routeUiHintTimerRef = useRef<number | null>(null);
  const [suggestedSource, setSuggestedSource] = useState<{
    id: string;
    name: string;
    confidence: number;
    reason?: string;
  } | null>(null);
  // ── Auto-routing toggle (default: on)
  const [autoRouting, setAutoRouting] = useState(true);
  const routeMetaRef = useRef<{
    route_confidence?: number;
    routing_method?: string;
    semantic_context?: Record<string, unknown> | unknown[];
  } | null>(null);
  // ── Panel state
  const [schemaDrawerOpen, setSchemaDrawerOpen] = useState(false);
  const [leftPanelCollapsed, setLeftPanelCollapsed] = useState(false);
  const [feedbackReasonByTurn, setFeedbackReasonByTurn] = useState<Record<string, string | null>>({});
  const [advancedSettingsOpen, setAdvancedSettingsOpen] = useState(false);
  const [referencePopoverOpen, setReferencePopoverOpen] = useState(false);
  const [selectedReferencePackIds, setSelectedReferencePackIds] = useState<string[]>([]);
  const [selectedReferenceFileIds, setSelectedReferenceFileIds] = useState<string[]>([]);
  const [includeAllReferences, setIncludeAllReferences] = useState(false);
  const [uploadingReferencePackId, setUploadingReferencePackId] = useState<string | null>(null);
  const [savingKnowledgeTurnIds, setSavingKnowledgeTurnIds] = useState<Record<string, boolean>>({});

  const SLASH_COMMANDS: Array<{
    key: SlashCommandKey;
    label: string;
    desc: string;
    usage: string;
  }> = useMemo(() => ([
    {
      key: 'multi',
      label: '/multi',
      desc: t('nl2sql.slashCommandMultiDesc'),
      usage: '/multi ',
    },
  ]), [t]);

  const inputRef = useRef<ElementRef<typeof TextArea>>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const queryTaskUnsubMapRef = useRef<Record<string, () => void>>({});
  const agentTaskUnsubMapRef = useRef<Record<string, () => void>>({});
  const routeTaskUnsubMapRef = useRef<Record<string, () => void>>({});
  const ROUTE_TASK_UI_TIMEOUT_MS = 450_000;
  const normalizeRouteTaskErrorMessage = useCallback((raw: string): string => {
    const text = String(raw ?? '').trim();
    if (!text) return text;
    const lower = text.toLowerCase();
    if (lower.includes('route task hard timeout')) {
      return t('nl2sql.routeTaskServerTimeout');
    }
    if (lower === 'route task timeout') {
      return t('nl2sql.routeTaskServerTimeout');
    }
    if (lower.includes('route task ui timeout')) {
      return t('nl2sql.routeTaskTimeout', { seconds: Math.floor(ROUTE_TASK_UI_TIMEOUT_MS / 1000) });
    }
    return text;
  }, [ROUTE_TASK_UI_TIMEOUT_MS, t]);

  const scrollToBottom = () => messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });

  useEffect(() => () => {
    Object.values(queryTaskUnsubMapRef.current).forEach((fn) => {
      try { fn(); } catch { /* noop */ }
    });
    queryTaskUnsubMapRef.current = {};
    Object.values(agentTaskUnsubMapRef.current).forEach((fn) => {
      try { fn(); } catch { /* noop */ }
    });
    agentTaskUnsubMapRef.current = {};
    Object.values(routeTaskUnsubMapRef.current).forEach((fn) => {
      try { fn(); } catch { /* noop */ }
    });
    routeTaskUnsubMapRef.current = {};
    if (routeUiHintTimerRef.current != null) {
      try { window.clearTimeout(routeUiHintTimerRef.current); } catch { /* noop */ }
      routeUiHintTimerRef.current = null;
    }
    Object.values(clarifyTaskUnsubMapRef.current).forEach((fn) => {
      try { fn(); } catch { /* noop */ }
    });
    clarifyTaskUnsubMapRef.current = {};
  }, []);

  // ── Queries
  const { data: dataSourcesData, isLoading: dsLoading } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list(),
    staleTime: 60_000,
  });

  // Single-mode UX: keep explicit selection optional for everyone.
  // Route engine + commands (`/ds`, `/multi`) decide by default.

  // F-12: Schema columns for spell correction
  const { data: schemaData } = useQuery({
    queryKey: queryKeys.dataSources.detail(selectedDataSourceId ?? ''),
    queryFn: () => dataSourcesApi.get(selectedDataSourceId ?? ''),
    enabled: !!selectedDataSourceId,
    staleTime: 60_000,
  });

  const { data: savedViewsData, refetch: refetchSavedViews } = useQuery({
    queryKey: queryKeys.nl2sql.views(),
    queryFn: () => nl2sqlApi.listViews(),
    staleTime: 60_000,
  });

  const getTurnViewTab = useCallback((turnId: string): ViewTab => {
    return turnViewTabs[turnId] ?? 'table';
  }, [turnViewTabs]);

  const setTurnViewTab = useCallback((turnId: string, tab: ViewTab) => {
    setTurnViewTabs((prev) => ({ ...prev, [turnId]: tab }));
  }, []);

  const renameViewMutation = useMutation({
    mutationFn: ({ id, data }: { id: string; data: { name?: string; description?: string } }) =>
      nl2sqlApi.updateView(id, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.views() });
      message.success(t('nl2sql.viewRenamed'));
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const deleteViewMutation = useMutation({
    mutationFn: (id: string) => nl2sqlApi.deleteView(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.views() });
      message.success(t('nl2sql.viewDeleted'));
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('nl2sql.deleteViewFailed'));
    },
  });

  // ── Embedding config: drives whether auto-routing is available.
  const { data: embeddingConfig } = useQuery({
    queryKey: queryKeys.nl2sql.embeddingConfig(),
    queryFn: () => nl2sqlApi.getEmbeddingConfig(),
    staleTime: 60_000,
  });
  const embeddingAvailable = embeddingConfig?.available === true;
  const embeddingRequiredBlocked = embeddingConfig !== undefined && !embeddingConfig.available;

  const { data: semanticsData } = useQuery({
    queryKey: ['nl2sql', 'semantics', selectedDataSourceId],
    queryFn: () => nl2sqlApi.getSemantics(selectedDataSourceId!),
    enabled: !!selectedDataSourceId,
    staleTime: 60_000,
  });

  const { data: referencePacks = EMPTY_REFERENCE_PACKS, isLoading: referencePacksLoading } = useQuery({
    queryKey: queryKeys.nl2sql.referencePacks(selectedDataSourceId ?? ''),
    queryFn: () => nl2sqlApi.listReferencePacks(selectedDataSourceId!),
    enabled: !!selectedDataSourceId,
    staleTime: 30_000,
  });

  useEffect(() => {
    setSelectedReferencePackIds([]);
    setSelectedReferenceFileIds([]);
    setIncludeAllReferences(false);
    setReferencePopoverOpen(false);
  }, [selectedDataSourceId]);

  useEffect(() => {
    const enabledPackIds = new Set(referencePacks.filter((pack) => pack.enabled).map((pack) => pack.id));
    const enabledFileIds = new Set(
      referencePacks
        .filter((pack) => pack.enabled)
        .flatMap((pack) => pack.files.map((file) => file.id)),
    );
    setSelectedReferencePackIds((prev) => prev.filter((id) => enabledPackIds.has(id)));
    setSelectedReferenceFileIds((prev) => prev.filter((id) => enabledFileIds.has(id)));
  }, [referencePacks]);

  const referencePackNameById = useMemo(() => {
    const map = new Map<string, string>();
    referencePacks.forEach((pack) => map.set(pack.id, pack.name));
    return map;
  }, [referencePacks]);

  const referenceFileNameById = useMemo(() => {
    const map = new Map<string, string>();
    referencePacks.forEach((pack) => pack.files.forEach((file) => map.set(file.id, file.filename)));
    return map;
  }, [referencePacks]);

  const selectedReferenceSummary = useMemo(() => {
    if (includeAllReferences) return t('nl2sql.references.allSelected');
    const count = selectedReferencePackIds.length + selectedReferenceFileIds.length;
    if (count === 0) return t('nl2sql.references.noneSelected');
    return t('nl2sql.references.selectedCount', { count });
  }, [includeAllReferences, selectedReferenceFileIds.length, selectedReferencePackIds.length, t]);
  const referenceOverrideActive =
    includeAllReferences || selectedReferencePackIds.length > 0 || selectedReferenceFileIds.length > 0;

  const buildReferenceBindings = useCallback((): Nl2sqlReferenceBindings | undefined => {
    if (includeAllReferences) return { includeAll: true, packIds: [], fileIds: [] };
    if (selectedReferencePackIds.length === 0 && selectedReferenceFileIds.length === 0) {
      return undefined;
    }
    return {
      includeAll: false,
      packIds: selectedReferencePackIds,
      fileIds: selectedReferenceFileIds,
    };
  }, [includeAllReferences, selectedReferenceFileIds, selectedReferencePackIds]);

  const createReferencePackMutation = useMutation({
    mutationFn: (name: string) => {
      if (!selectedDataSourceId) throw new Error(t('nl2sql.selectDataSourceFirst'));
      return nl2sqlApi.createReferencePack({ datasourceId: selectedDataSourceId, name });
    },
    onSuccess: () => {
      if (selectedDataSourceId) {
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.referencePacks(selectedDataSourceId) });
      }
      message.success(t('nl2sql.references.packCreated'));
    },
    onError: (err: unknown) => {
      message.error(err instanceof Error ? err.message : t('nl2sql.references.packCreateFailed'));
    },
  });

  const uploadReferenceMutation = useMutation({
    mutationFn: ({ packId, file }: { packId: string; file: File }) =>
      nl2sqlApi.uploadReferenceFile(packId, file),
    onMutate: ({ packId }) => {
      setUploadingReferencePackId(packId);
    },
    onSuccess: () => {
      if (selectedDataSourceId) {
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.referencePacks(selectedDataSourceId) });
      }
      message.success(t('nl2sql.references.fileUploaded'));
    },
    onError: (err: unknown) => {
      message.error(err instanceof Error ? err.message : t('nl2sql.references.fileUploadFailed'));
    },
    onSettled: () => {
      setUploadingReferencePackId(null);
    },
  });

  const updateReferencePackMutation = useMutation({
    mutationFn: ({ pack, enabled }: { pack: Nl2sqlReferencePack; enabled: boolean }) =>
      nl2sqlApi.updateReferencePack(pack.id, { enabled }),
    onSuccess: (pack) => {
      if (selectedDataSourceId) {
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.referencePacks(selectedDataSourceId) });
      }
      if (!pack.enabled) {
        setSelectedReferencePackIds((prev) => prev.filter((id) => id !== pack.id));
        setSelectedReferenceFileIds((prev) => prev.filter((id) => !pack.files.some((file) => file.id === id)));
      }
      message.success(pack.enabled ? t('nl2sql.references.packEnabled') : t('nl2sql.references.packDisabled'));
    },
    onError: (err: unknown) => {
      message.error(err instanceof Error ? err.message : t('nl2sql.references.packUpdateFailed'));
    },
  });

  const deleteReferencePackMutation = useMutation({
    mutationFn: (pack: Nl2sqlReferencePack) => nl2sqlApi.deleteReferencePack(pack.id).then(() => pack),
    onSuccess: (pack) => {
      if (selectedDataSourceId) {
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.referencePacks(selectedDataSourceId) });
      }
      setSelectedReferencePackIds((prev) => prev.filter((id) => id !== pack.id));
      setSelectedReferenceFileIds((prev) => prev.filter((id) => !pack.files.some((file) => file.id === id)));
      message.success(t('nl2sql.references.packDeleted'));
    },
    onError: (err: unknown) => {
      message.error(err instanceof Error ? err.message : t('nl2sql.references.packDeleteFailed'));
    },
  });

  const deleteReferenceFileMutation = useMutation({
    mutationFn: (fileId: string) => nl2sqlApi.deleteReferenceFile(fileId).then(() => fileId),
    onSuccess: (fileId) => {
      if (selectedDataSourceId) {
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.referencePacks(selectedDataSourceId) });
      }
      setSelectedReferenceFileIds((prev) => prev.filter((id) => id !== fileId));
      message.success(t('nl2sql.references.fileDeleted'));
    },
    onError: (err: unknown) => {
      message.error(err instanceof Error ? err.message : t('nl2sql.references.fileDeleteFailed'));
    },
  });

  // Sync auto-routing with embedding availability:
  // autoRouting is only meaningful when an embedding model is configured.
  // Re-enable autoRouting when embedding becomes available so the user
  // doesn't need to manually toggle it after fixing the config.
  // Track a pending warning promise so consecutive effect passes (StrictMode) share the
  // same toast instead of queuing two. We keep a ref so the closure captures the latest value.
  const pendingWarningRef = useRef<Promise<unknown> | null>(null);

  useEffect(() => {
    if (embeddingConfig === undefined) return; // still loading

    if (!embeddingConfig?.available && autoRouting) {
      // If a warning is already showing, just disable routing — don't queue another toast.
      // If StrictMode re-runs this effect before the first toast resolves, we await the
      // pending promise so ant-design deduplicates into a single notification.
      if (pendingWarningRef.current) {
        setAutoRouting(false);
        return;
      }
      pendingWarningRef.current = Promise.resolve(message
        .warning(t('nl2sql.embeddingNotConfigured')))
        .then(() => {
          pendingWarningRef.current = null;
        });
      setAutoRouting(false);
    }
    if (embeddingConfig?.available && !autoRouting) {
      setAutoRouting(true);
    }
  }, [embeddingConfig]);

  // P3-1: Restore pending clarification state on page load (page refresh recovery).
  useEffect(() => {
    if (dataSourcesData?.data_sources?.length === 0) return; // wait for data sources to load
    nl2sqlApi.getClarify(sessionId).then(res => {
      if (res?.pending_clarification) {
        const genTurnId = `gen-${Date.now()}`;
        const genTurn: NlTurn = {
          id: genTurnId,
          role: 'assistant',
          question: res.pending_clarification.original_question,
          dataSourceId: '',
          isGenerating: false,
        };
        setTurns(prev => [...prev, genTurn]);
        setClarificationContext(res.pending_clarification);
        setClarifyingTurnId(genTurnId);
      }
    }).catch(() => {
      // Ignore — no pending clarification on fresh load
    });
  }, [dataSourcesData?.data_sources?.length, sessionId]);

  const dataSources: DataSourceInfo[] = useMemo(() => {
    return (dataSourcesData?.data_sources ?? []).filter((ds: DataSourceInfo) => ds.enabled !== false);
  }, [dataSourcesData]);

  const selectedDataSource = dataSources.find(ds => ds.id === selectedDataSourceId);

  const savedViews = useMemo(() => {
    return ((savedViewsData as any)?.views ?? []).map((v: any) => ({
      id: v.query_id,
      query_id: v.query_id,
      conversation_id: v.conversation_id ?? null,
      name: v.name,
      question: v.description ?? v.name,
      sql: v.sql,
      data_source_id: v.data_source_id ?? null,
      description: v.description,
      created_at: v.created_at,
    }));
  }, [savedViewsData]);

  // Input should not trigger routing. We only clear stale suggestions here;
  // actual routing happens once when the user sends the question.
  useEffect(() => {
    const trimmed = input.trim();
    if (trimmed.startsWith('/')) {
      routeMetaRef.current = null;
      setSuggestedSource(null);
      setRoutingStage('idle');
      return;
    }
    if (selectedDataSourceId) {
      routeMetaRef.current = null;
      setSuggestedSource(null);
      setRoutingStage('idle');
      return;
    }
    routeMetaRef.current = null;
    setSuggestedSource(null);
    setRoutingStage('idle');
  }, [input, selectedDataSourceId]);

  // ── Generate SQL + execute (single task processor; queue consumer calls this)
  const executeQueryViaTask = useCallback(async (
    dsId: string,
    question: string,
    generatingTurnId: string,
    routeMeta?: {
      route_confidence?: number;
      routing_method?: string;
      semantic_context?: Record<string, unknown> | unknown[];
    },
    referenceBindings?: Nl2sqlReferenceBindings,
  ): Promise<Nl2sqlQueryResponse> => {
    const start = await nl2sqlApi.queryAsync({
      data_source_id: dsId,
      question,
      conversation_id: conversationId ?? undefined,
      route_confidence: routeMeta?.route_confidence,
      routing_method: routeMeta?.routing_method,
      semantic_context: routeMeta?.semantic_context,
      reference_bindings: referenceBindings,
    });
    const taskId = start.taskId;
    setTurns(prev => prev.map(turn => turn.id === generatingTurnId ? {
      ...turn,
      queryTaskId: taskId,
      queryStage: 'queued',
      queryStageMessage: t('nl2sql.routeMessages.queuedEntered'),
      queryElapsedMs: Math.max(turn.queryElapsedMs ?? 0, 0),
      queryStageTimeline: appendOrUpdateStageTimeline(
        turn.queryStageTimeline,
        'queued',
        t('nl2sql.routeMessages.queuedEntered'),
        0,
        0,
      ),
    } : turn));

    const finalEvent = await new Promise<Nl2sqlQueryTaskEvent>((resolve, reject) => {
      const cleanup = () => {
        const unsub = queryTaskUnsubMapRef.current[taskId];
        if (unsub) {
          try { unsub(); } catch { /* noop */ }
          delete queryTaskUnsubMapRef.current[taskId];
        }
      };
      const unsub = streamNl2sqlQueryTask(taskId, {
        onEvent: (evt) => {
          if (!mountedRef.current) return;
          setTurns(prev => prev.map((turn) => turn.id === generatingTurnId ? {
            ...(turn.queryStage === evt.stage
              ? {}
              : {
                  queryStageHistory: [
                    ...(turn.queryStageHistory ?? []),
                    ...(evt.stage ? [evt.stage] : []),
                  ],
                }),
            ...turn,
            queryTaskId: evt.task_id,
            queryStage: evt.stage ?? null,
            queryStageMessage: resolveRouteStageMessage(t, evt.stage, evt.message),
            queryElapsedMs: evt.elapsed_ms ?? 0,
            queryStageTimeline: evt.stage ? (() => {
              const current = turn.queryStageTimeline ?? [];
              const idx = current.findIndex(item => item.stage === evt.stage);
              if (idx >= 0) {
                const cloned = [...current];
                cloned[idx] = {
                  ...cloned[idx],
                  message: resolveRouteStageMessage(t, evt.stage, evt.message)
                    ?? cloned[idx].message
                    ?? null,
                  atElapsedMs: evt.elapsed_ms ?? cloned[idx].atElapsedMs ?? 0,
                  stageElapsedMs: evt.stage_elapsed_ms ?? cloned[idx].stageElapsedMs ?? null,
                };
                return cloned;
              }
              return [
                ...current,
                {
                  stage: evt.stage,
                  message: resolveRouteStageMessage(t, evt.stage, evt.message),
                  atElapsedMs: evt.elapsed_ms ?? 0,
                  stageElapsedMs: evt.stage_elapsed_ms ?? null,
                },
              ];
            })() : (turn.queryStageTimeline ?? []),
          } : turn));

          if (
            evt.status === 'completed'
            || evt.status === 'failed'
            || evt.status === 'clarification_needed'
          ) {
            cleanup();
            resolve(evt);
          }
        },
        onError: (err) => {
          cleanup();
          reject(new Error(err));
        },
      });
      queryTaskUnsubMapRef.current[taskId] = unsub;
    });

    if (finalEvent.response) return finalEvent.response;
    if (finalEvent.error) throw new Error(finalEvent.error);
    throw new Error('query task ended without response');
  }, [conversationId, t]);

  const runQuestion = async (question: string, optimisticUserTurnId?: string) => {
    if (!question.trim()) return;

    if (embeddingRequiredBlocked) {
      message.warning(t('nl2sql.embeddingRequiredForExplore'));
      return;
    }

    if (dataSources.length === 0) {
      message.warning(t('nl2sql.noDataSourcesConfigured'));
      return;
    }

    if (isStandaloneGreeting(question) || isPunctuationOnly(question)) {
      const turnId = optimisticUserTurnId ?? `turn-${Date.now()}`;
      if (!optimisticUserTurnId) {
        setTurns((prev) => [...prev, { id: turnId, role: 'user', question }]);
      }
      setTurns((prev) => [
        ...prev,
        {
          id: `clarify-${turnId}`,
          role: 'assistant',
          question,
          isGenerating: false,
          clarificationQuestion: isStandaloneGreeting(question)
            ? t('nl2sql.nonDataGreetingPrompt')
            : t('nl2sql.nonDataInputPrompt'),
        },
      ]);
      setTimeout(scrollToBottom, 50);
      return;
    }

    // Slash commands:
    // /multi <question>  -> force cross-datasource agent plan
    const lower = question.toLowerCase();
    if (lower.startsWith('/multi')) {
      const forcedQuestion = question.replace(/^\/multi\s*/i, '').trim();
      if (!forcedQuestion) {
        message.warning('请在 /multi 后输入问题，例如：/multi 统计近7天各租户token消耗趋势');
        return;
      }
      await handleAutoMultiSource(forcedQuestion);
      return;
    }

    const turnId = optimisticUserTurnId ?? `turn-${Date.now()}`;
    const generatingTurnId = `gen-${turnId}`;

    if (!optimisticUserTurnId) {
      const userTurn: NlTurn = {
        id: turnId,
        role: 'user',
        question,
        dataSourceId: selectedDataSourceId ?? suggestedSource?.id ?? undefined,
      };
      setTurns(prev => [...prev, userTurn]);
    }

    // AUTO routing:
    // 1) multi-datasource request -> execute via agent planner
    // 2) clarification needed -> ask until clarified
    // 3) otherwise resolve single datasource and run normal /query flow
    const hadExplicitDataSource = Boolean(selectedDataSourceId);
    let dsId = selectedDataSourceId ?? suggestedSource?.id ?? null;
    let routedAsMulti = false;
    let routeClarificationNeeded = false;
    let routeTerminalError: string | null = null;
    let routeMeta = routeMetaRef.current ?? undefined;
    const referenceBindings = buildReferenceBindings();

    const appendStageTimeline = (
      timeline: NlTurn['queryStageTimeline'],
      stage: string,
      messageText: string | null,
      elapsed: number,
      stageElapsed: number | null,
    ) => {
      const current = timeline ?? [];
      const idx = current.findIndex((item) => item.stage === stage);
      if (idx >= 0) {
        const cloned = [...current];
        cloned[idx] = {
          ...cloned[idx],
          message: messageText ?? cloned[idx].message ?? null,
          atElapsedMs: Math.max(cloned[idx].atElapsedMs, elapsed),
          stageElapsedMs: stageElapsed ?? cloned[idx].stageElapsedMs ?? null,
        };
        return cloned;
      }
      return [
        ...current,
        {
          stage,
          message: messageText ?? null,
          atElapsedMs: elapsed,
          stageElapsedMs: stageElapsed ?? null,
        },
      ];
    };

    const upsertRoutingTurn = (
      stage: string,
      stageMessage: string,
      elapsedMs: number,
      stageElapsedMs?: number | null,
    ) => {
      setTurns(prev => {
        const idx = prev.findIndex((t) => t.id === generatingTurnId);
        if (idx >= 0) {
          return prev.map((t) => t.id === generatingTurnId ? {
            ...t,
            isGenerating: true,
            question,
            queryStage: stage,
            queryStageMessage: stageMessage,
            queryElapsedMs: elapsedMs,
            queryStageTimeline: appendStageTimeline(
              t.queryStageTimeline,
              stage,
              stageMessage,
              elapsedMs,
              stageElapsedMs ?? null,
            ),
          } : t);
        }
        const generated: NlTurn = {
          id: generatingTurnId,
          role: 'assistant',
          question,
          dataSourceId: dsId ?? undefined,
          isGenerating: true,
          queryStage: stage,
          queryStageMessage: stageMessage,
          queryElapsedMs: elapsedMs,
          queryStageTimeline: [{
            stage,
            message: stageMessage,
            atElapsedMs: elapsedMs,
            stageElapsedMs: stageElapsedMs ?? null,
          }],
        };
        return [...prev, generated];
      });
    };

    if (!dsId) {
      upsertRoutingTurn('request_validation', t('nl2sql.routeMessages.requestValidationStart'), 0, 0);
      setTimeout(scrollToBottom, 50);
      setIsSuggestingSource(true);
      if (routeUiHintTimerRef.current != null) {
        window.clearTimeout(routeUiHintTimerRef.current);
        routeUiHintTimerRef.current = null;
      }
      routeUiHintTimerRef.current = window.setTimeout(() => {
        if (!mountedRef.current) return;
        setRoutingStage((prev) => (prev === 'search_candidates' || prev === 'ai_confirming' ? 'manual_continue' : prev));
      }, 10_000);
      try {
        const start = await nl2sqlApi.routeAsync({ question });
        const taskId = start.taskId;
        const finalEvent = await new Promise<any>((resolve, reject) => {
          let settled = false;
          const cleanup = () => {
            const unsub = routeTaskUnsubMapRef.current[taskId];
            if (unsub) {
              try { unsub(); } catch { /* noop */ }
              delete routeTaskUnsubMapRef.current[taskId];
            }
          };
          const finalize = (evt: any, nextStage: 'ready' | 'manual_continue') => {
            if (settled) return;
            settled = true;
            window.clearTimeout(timeoutId);
            cleanup();
            if (routeUiHintTimerRef.current != null) {
              window.clearTimeout(routeUiHintTimerRef.current);
              routeUiHintTimerRef.current = null;
            }
            setRoutingStage(nextStage);
            setIsSuggestingSource(false);
            resolve(evt);
          };
          const timeoutId = window.setTimeout(() => {
            if (settled) return;
            settled = true;
            cleanup();
            reject(new Error('route task ui timeout'));
          }, ROUTE_TASK_UI_TIMEOUT_MS);
          const unsub = streamNl2sqlRouteTask(taskId, {
            onEvent: (evt) => {
              if (!mountedRef.current) return;
              if (evt.stage) {
                if (evt.stage === 'manual_continue') setRoutingStage('manual_continue');
                else if (evt.stage === 'ready' || evt.stage === 'done' || evt.stage === 'route_selected') setRoutingStage('ready');
                else if (evt.stage === 'ai_confirming' || evt.stage === 'llm_routing' || evt.stage === 'domain_classifying') setRoutingStage('ai_confirming');
                else setRoutingStage('search_candidates');
              }
              if (evt.stage) {
                upsertRoutingTurn(
                  evt.stage,
                  resolveRouteStageMessage(t, evt.stage, evt.message),
                  evt.elapsed_ms ?? 0,
                  evt.stage_elapsed_ms ?? null,
                );
              }
              if (evt.response?.result) {
                const r = evt.response.result;
                routeMeta = {
                  route_confidence: Number.isFinite(r.confidence) ? r.confidence : undefined,
                  routing_method: r.method,
                  semantic_context: buildSemanticContextFromMatchedTables(r.matched_tables ?? []),
                };
                routeMetaRef.current = routeMeta;
                if (r.method === 'cross-datasource' || r.data_source_id === 'multi-datasource') {
                  routedAsMulti = true;
                } else if (r.method === 'clarification' && r.clarification_question) {
                  if (settled) return;
                  settled = true;
                  routeClarificationNeeded = true;
                  const ctx: ClarificationContextType = {
                    original_question: question,
                    clarification_question: r.clarification_question,
                    options: (r.matched_tables ?? []).map((mt, i) => ({
                      option_index: i,
                      data_source_id: mt.data_source_id,
                      table_name: mt.table_name,
                      column_name: mt.best_column,
                      reason: mt.column_description,
                      sim_score: mt.similarity_score,
                    })),
                    turn: 0,
                    conversation_id: conversationId ?? '',
                  };
                  setClarificationContext(ctx);
                  setClarifyingTurnId(generatingTurnId);
                  setTurns(prev => prev.map((t) => t.id === generatingTurnId ? {
                    ...t,
                    isGenerating: false,
                    clarificationQuestion: r.clarification_question,
                    clarificationConfirmedRequirements: null,
                    clarificationMissingRequirements: null,
                  } : t));
                  setIsSuggestingSource(false);
                  setSuggestedSource(null);
                  window.clearTimeout(timeoutId);
                  cleanup();
                  resolve(evt);
                } else {
                  const ds = dataSources.find((d) => d.id === r.data_source_id);
                  if (ds) {
                    dsId = r.data_source_id ?? null;
                    setSuggestedSource({
                      id: r.data_source_id ?? '',
                      name: ds.name,
                      confidence: r.confidence,
                      reason: r.method,
                    });
                  }
                  finalize(evt, 'ready');
                }
              }
              if (evt.error) {
                routeMeta = undefined;
                routeMetaRef.current = null;
                routeTerminalError = normalizeRouteTaskErrorMessage(evt.error);
              }
              if (evt.status === 'failed' || evt.status === 'clarification_needed' || evt.status === 'completed') {
                finalize(evt, evt.status === 'completed' ? 'ready' : 'manual_continue');
              }
            },
            onDone: (evt) => {
              finalize(evt, evt.status === 'failed' || evt.status === 'clarification_needed' ? 'manual_continue' : 'ready');
            },
            onError: (err) => {
              if (settled) return;
              settled = true;
              window.clearTimeout(timeoutId);
              cleanup();
              reject(new Error(err));
            },
          });
          routeTaskUnsubMapRef.current[taskId] = unsub;
        });
        if (finalEvent?.error) {
          routeMeta = undefined;
          routeMetaRef.current = null;
          routeTerminalError = normalizeRouteTaskErrorMessage(finalEvent.error);
        }
      } catch (routeErr) {
        if (routeUiHintTimerRef.current != null) {
          window.clearTimeout(routeUiHintTimerRef.current);
          routeUiHintTimerRef.current = null;
        }
        const normalized = normalizeRouteTaskErrorMessage((routeErr as Error)?.message ?? '');
        setRoutingStage('manual_continue');
        routeMeta = undefined;
        routeMetaRef.current = null;
        setTurns(prev => prev.map((t) => t.id === generatingTurnId ? {
          ...t,
          isGenerating: false,
          error: normalized,
        } : t));
        // Route failed; fall back below.
      } finally {
        if (routeUiHintTimerRef.current != null) {
          window.clearTimeout(routeUiHintTimerRef.current);
          routeUiHintTimerRef.current = null;
        }
        setIsSuggestingSource(false);
        if (!selectedDataSourceId && !suggestedSource) {
          setRoutingStage((prev) => (prev === 'ready' ? prev : 'manual_continue'));
        }
      }
    }

    if (routeClarificationNeeded) {
      return;
    }

    if (routeTerminalError) {
      const noMatchMessage = t('nl2sql.noSemanticCandidatePrompt');
      setTurns(prev => prev.map((turn) => turn.id === generatingTurnId ? {
        ...turn,
        isGenerating: false,
        queryStage: 'failed',
        queryStageMessage: noMatchMessage,
        clarificationQuestion: noMatchMessage,
        error: null,
      } : turn));
      setRoutingStage('manual_continue');
      setTimeout(scrollToBottom, 50);
      return;
    }

    if (routedAsMulti) {
      setTurns(prev => prev.filter((t) => t.id !== generatingTurnId));
      await handleAutoMultiSource(question);
      return;
    }

    if (!dsId && dataSources.length === 1) {
      dsId = dataSources[0].id;
      routeMeta = undefined;
      routeMetaRef.current = null;
      message.warning(t('nl2sql.routingUnconfidentFallback', { name: dataSources[0].name }));
    }
    if (!dsId) {
      const noMatchMessage = routeMetaRef.current ? t('nl2sql.noRoutingMatch') : t('nl2sql.pickOrDescribeDataSource');
      setTurns(prev => prev.map((t) => t.id === generatingTurnId ? {
        ...t,
        isGenerating: false,
        error: noMatchMessage,
      } : t));
      message.warning(noMatchMessage);
      return;
    }

    if (hadExplicitDataSource) {
      routeMeta = undefined;
      routeMetaRef.current = null;
    }
    setTurns(prev => prev.map(t => t.id === turnId
      ? { ...t, dataSourceId: dsId ?? undefined }
      : t));
    setTurns(prev => {
      const exists = prev.some((t) => t.id === generatingTurnId);
      if (!exists) {
        const generated: NlTurn = {
          id: generatingTurnId,
          role: 'assistant',
          question,
          dataSourceId: dsId ?? undefined,
          isGenerating: true,
          queryStage: 'queued',
          queryStageMessage: t('nl2sql.routeMessages.prepareSendRequest'),
          queryElapsedMs: 0,
          queryStageTimeline: [{
            stage: 'queued',
            message: t('nl2sql.routeMessages.prepareSendRequest'),
            atElapsedMs: 0,
            stageElapsedMs: 0,
          }],
          routeConfidence: routeMeta?.route_confidence ?? null,
          routingMethod: routeMeta?.routing_method ?? null,
          semanticContext: routeMeta?.semantic_context ?? null,
        };
        return [...prev, generated];
      }
      return prev.map((turn) => turn.id === generatingTurnId ? {
        ...turn,
        question,
        dataSourceId: dsId ?? undefined,
        isGenerating: true,
        queryStage: 'queued',
        queryStageMessage: t('nl2sql.routeMessages.prepareSendRequest'),
        queryElapsedMs: Math.max(turn.queryElapsedMs ?? 0, 0),
        queryStageTimeline: appendStageTimeline(
          turn.queryStageTimeline,
          'queued',
          t('nl2sql.routeMessages.prepareSendRequest'),
          0,
          0,
        ),
        routeConfidence: routeMeta?.route_confidence ?? null,
        routingMethod: routeMeta?.routing_method ?? null,
        semanticContext: routeMeta?.semantic_context ?? null,
      } : turn);
    });
    setTimeout(scrollToBottom, 50);

    try {
      const res = await executeQueryViaTask(dsId, question, generatingTurnId, routeMeta, referenceBindings);

      const normalizedErr = normalizeNl2sqlErrorMessage(res.error ?? '', t);
      if (!res.sql && normalizedErr) {
        if (!hadExplicitDataSource) {
          setSuggestedSource(null);
        }
        setTurns(prev => prev.map(t => t.id === generatingTurnId ? {
          ...t,
          error: normalizedErr,
          queryId: res.queryId,
          appliedRules: mergeAppliedRules(t.appliedRules, res.appliedRules),
          usedReferences: res.usedReferences ?? t.usedReferences,
          isGenerating: false,
        } : t));
        message.error(normalizedErr, 6);
        setTimeout(scrollToBottom, 50);
        return;
      }

      const sql = res.sql ?? '';
      const explanation = res.explanation ?? '';
      const queryId = res.queryId;
      const newConversationId = res.conversationId;

      if (!conversationId && newConversationId) {
        setConversationId(newConversationId);
      }

      // Handle clarification request from LLM
      if (res.clarificationQuestion) {
        const clarifyCtx: ClarificationContextType = {
          original_question: question,
          clarification_question: res.clarificationQuestion,
          options: [
            {
              option_index: 0,
              data_source_id: dsId,
              table_name: '当前数据源',
              column_name: '补充需求',
              reason: '请补充缺失条件后继续',
              sim_score: 1,
              business_meaning: '继续输入缺失的关键业务约束',
            },
          ],
          confirmed_requirements: res.confirmedRequirements ?? undefined,
          missing_requirements: res.missingRequirements ?? undefined,
          turn: 0,
          conversation_id: newConversationId ?? (conversationId ?? ''),
        };
        setClarificationContext(clarifyCtx);
        setClarifyingTurnId(generatingTurnId);
        setTurns(prev => prev.map(t => t.id === generatingTurnId ? {
          ...t,
          sql: '',
          explanation: '',
          queryId,
          appliedRules: mergeAppliedRules(t.appliedRules, res.appliedRules),
          usedReferences: res.usedReferences ?? t.usedReferences,
          isGenerating: false,
          clarificationQuestion: res.clarificationQuestion,
          clarificationConfirmedRequirements: res.confirmedRequirements ?? t.clarificationConfirmedRequirements ?? null,
          clarificationMissingRequirements: res.missingRequirements ?? t.clarificationMissingRequirements ?? null,
        } : t));
        setTimeout(scrollToBottom, 50);
        return;
      }

      if (!hadExplicitDataSource) {
        setSelectedDataSourceId(dsId);
        setSuggestedSource(null);
      }

      // Update generating turn with SQL and query_id
      setTurns(prev => prev.map(t => t.id === generatingTurnId ? {
        ...t,
        sql,
        explanation,
        queryId,
        appliedRules: mergeAppliedRules(t.appliedRules, res.appliedRules),
        usedReferences: res.usedReferences ?? t.usedReferences,
        isGenerating: false,
        isExecuting: true,
        autoExecuteFailed: false,
      } : t));

      // P3-Enterprise: parallel-fetch Query Understanding so it's ready when user sees results.
      // F-02: Guard setTurns against unmounted component.
      nl2sqlApi.queryUnderstanding(dsId, question).then((qu) => {
        if (mountedRef.current) {
          setTurns(prev => prev.map(t => t.id === generatingTurnId ? {
            ...t,
            queryUnderstanding: qu,
          } : t));
        }
      }).catch(() => {
        // QU is best-effort; non-fatal
      });

      setTimeout(scrollToBottom, 50);

      await executeSqlForTurn({
        turnId: generatingTurnId,
        sql,
        queryId,
        dataSourceId: dsId,
        pager: { page: 1, pageSize: DEFAULT_AUTO_EXECUTE_PAGE_SIZE },
        auto: true,
      });
      setTimeout(scrollToBottom, 50);
    } catch (genErr) {
      const normalizedError = normalizeNl2sqlErrorMessage((genErr as Error).message, t);
      if (!hadExplicitDataSource) {
        setSuggestedSource(null);
      }
      setTurns(prev => prev.map(t => t.id === generatingTurnId ? {
        ...t,
        error: normalizedError,
        isGenerating: false,
      } : t));
      message.error(`${t('nl2sql.generateFailed')}: ${normalizedError}`);
    }
  };

  const {
    items: queuedPrompts,
    processing: isQueueProcessing,
    activeItemId: activeQueueItemId,
    enqueue: enqueuePrompt,
    updateItem: updateQueuedPrompt,
    removeItem,
    clear: clearPromptQueue,
    resume: resumePromptQueue,
  } = usePromptQueue({
    paused: !!clarificationContext || clarificationPausedByUser,
    onProcess: async (item) => {
      await runQuestion(item.question, item.optimisticUserTurnId);
    },
  });

  const removeQueuedPrompt = useCallback((id: string) => {
    if (id === activeQueueItemId) return;
    removeItem(id);
  }, [activeQueueItemId, removeItem]);

  const handleSend = (overrideQuestion?: string) => {
    const hasOverride = typeof overrideQuestion === 'string';
    const question = (overrideQuestion ?? input).trim();
    if (!question) return;
    const userTurnId = `turn-${Date.now()}`;
    const userTurn: NlTurn = {
      id: userTurnId,
      role: 'user',
      question,
      dataSourceId: selectedDataSourceId ?? suggestedSource?.id ?? undefined,
    };
    setTurns(prev => [...prev, userTurn]);
    setTimeout(scrollToBottom, 30);
    if (!hasOverride) {
      setInput('');
    }
    enqueuePrompt(question, hasOverride ? 'retry' : 'input', userTurnId);
  };

  // ── SQL Safety Gate — detect high-risk DML/DDL and require confirmation ─────────
  const HIGH_RISK_PATTERN = /^\s*(INSERT\s+|UPDATE\s+|DELETE\s+|DROP\s+|TRUNCATE\s+|ALTER\s+|CREATE\s+|REPLACE\s+)/i;

  const confirmHighRiskSql = (sql: string): Promise<boolean> =>
    new Promise(resolve => {
      Modal.confirm({
        title: t('nl2sql.confirmExecuteHighRisk'),
        icon: <ExclamationCircleOutlined style={{ color: '#faad14' }} />,
        content: (
          <div>
            <p style={{ marginBottom: 8 }}>{t('nl2sql.highRiskWarning')}</p>
            <pre style={{
              background: 'var(--bg-secondary)',
              padding: 8,
              borderRadius: 4,
              fontSize: 11,
              maxHeight: 120,
              overflow: 'auto',
              fontFamily: 'var(--font-code)',
              wordBreak: 'break-all',
            }}>
              {sql.trim()}
            </pre>
          </div>
        ),
        okText: t('nl2sql.executeConfirm'),
        cancelText: t('common.cancel'),
        okButtonProps: { style: { color: '#fff' } },
        onOk: () => resolve(true),
        onCancel: () => resolve(false),
      });
    });

  const executeSqlForTurn = useCallback(async ({
    turnId,
    sql,
    queryId,
    dataSourceId,
    pager,
    auto = false,
  }: {
    turnId: string;
    sql: string;
    queryId: string;
    dataSourceId: string;
    pager: { page: number; pageSize: number };
    auto?: boolean;
  }) => {
    const sqlToExec = sql.trim();
    if (!sqlToExec) return;
    const effectiveDataSourceId = dataSourceId;
    if (!effectiveDataSourceId) {
      message.error(`${t('nl2sql.executeFailed')}: missing data source id`);
      return;
    }
    const effectiveQueryId = queryId.trim();
    if (!effectiveQueryId) {
      message.error(`${t('nl2sql.executeFailed')}: ${t('nl2sql.missingQueryId')}`);
      return;
    }

    // Safety Gate: require confirmation for high-risk DML/DDL
    if (HIGH_RISK_PATTERN.test(sqlToExec)) {
      const confirmed = await confirmHighRiskSql(sqlToExec);
      if (!confirmed) return;
    }

    setTurns(prev => prev.map(t => t.id === turnId ? {
      ...t,
      isExecuting: true,
      error: null,
      autoExecuteFailed: false,
    } : t));
    setTurnPager(turnId, pager.page, pager.pageSize);

    try {
      const execRes = await nl2sqlApi.execute({
        query_id: effectiveQueryId,
        sql: sqlToExec,
        data_source_id: effectiveDataSourceId,
      });
      setTurns(prev => prev.map(t => t.id === turnId ? {
        ...t,
        result: {
          columns: execRes.columns.map(c => typeof c === 'string' ? c : (c as any).name),
          rows: execRes.rows ?? [],
          row_count: execRes.rows_count ?? execRes.rows?.length ?? 0,
          total_rows: execRes.total_rows ?? 0,
          has_more: execRes.has_more ?? false,
          limit: execRes.limit ?? execRes.rows_count ?? execRes.rows?.length ?? 0,
          offset: execRes.offset ?? 0,
          execution_time_ms: execRes.execution_ms,
        },
        resultSource: 'query',
        appliedRules: mergeAppliedRules(t.appliedRules, execRes.applied_rules),
        resultScore: execRes.result_score ?? null,
        validationWarnings: execRes.warnings ?? null,
        validationSuggestions: execRes.suggestions ?? null,
        isExecuting: false,
        autoExecuteFailed: auto && !!execRes.error,
        error: execRes.error,
      } : t));
      setTurnViewTab(turnId, 'table');
    } catch (execErr) {
      const normalizedError = normalizeNl2sqlErrorMessage((execErr as Error).message, t);
      setTurns(prev => prev.map(t => t.id === turnId ? {
        ...t,
        error: normalizedError,
        isExecuting: false,
        autoExecuteFailed: auto,
      } : t));
    }
  }, [t, setTurnViewTab]);

  // ── Manual execute / re-run
  const handleExecute = useCallback(async (
    turnId: string,
    pagerOverride?: { page: number; pageSize: number },
  ) => {
    const turn = turns.find(t => t.id === turnId);
    if (!turn?.sql) return;
    const effectiveDataSourceId = turn.dataSourceId ?? selectedDataSourceId;
    const effectiveQueryId = resolveTurnQueryId(turn);
    if (!effectiveQueryId) {
      message.error(`${t('nl2sql.executeFailed')}: ${t('nl2sql.missingQueryId')}`);
      return;
    }
    await executeSqlForTurn({
      turnId,
      sql: turn.editedSql ?? turn.sql,
      queryId: effectiveQueryId,
      dataSourceId: effectiveDataSourceId ?? '',
      pager: pagerOverride ?? getTurnPager(turnId),
    });
  }, [turns, selectedDataSourceId, t, getTurnPager, executeSqlForTurn]);

  const fetchMultiSourceResultPage = useCallback(async (
    turnId: string,
    page: number,
    pageSize: number,
  ) => {
    const turn = turns.find((item) => item.id === turnId);
    const effectiveQueryId = resolveTurnQueryId(turn);
    if (!effectiveQueryId) {
      message.error(`${t('nl2sql.executeFailed')}: ${t('nl2sql.missingQueryId')}`);
      return;
    }

    setTurns((prev) => prev.map((item) => item.id === turnId ? {
      ...item,
      isExecuting: true,
      error: null,
    } : item));

    try {
      const pageRes = await nl2sqlApi.getAgentResultPage(effectiveQueryId, {
        page,
        per_page: pageSize,
      });
      setTurns((prev) => prev.map((item) => item.id === turnId ? {
        ...item,
        isExecuting: false,
        error: null,
        result: {
          columns: pageRes.columns ?? item.result?.columns ?? [],
          rows: pageRes.rows ?? [],
          row_count: pageRes.totalRows ?? pageRes.rows?.length ?? 0,
          total_rows: pageRes.totalRows ?? pageRes.rows?.length ?? 0,
          has_more: pageRes.hasMore ?? false,
          limit: pageRes.perPage ?? pageSize,
          offset: ((pageRes.page ?? page) - 1) * (pageRes.perPage ?? pageSize),
          execution_time_ms: item.result?.execution_time_ms,
        },
        resultSource: 'agent',
      } : item));
    } catch (err) {
      const errMsg = (err as Error).message;
      setTurns((prev) => prev.map((item) => item.id === turnId ? {
        ...item,
        isExecuting: false,
        error: errMsg,
      } : item));
      message.error(`${t('nl2sql.executeFailed')}: ${errMsg}`);
    }
  }, [turns, t]);

  const hydrateHistoricalResultPages = useCallback((items: NlTurn[]) => {
    items.forEach((item) => {
      const queryId = resolveTurnQueryId(item);
      if (!queryId || !item.result || item.result.rows.length > 0) return;
      const source = item.resultSource ?? (item.dataSourceId ? 'query' : 'agent');
      const params = { page: 1, per_page: item.result.limit ?? DEFAULT_TABLE_PAGE_SIZE };
      const fetcher = source === 'agent'
        ? nl2sqlApi.getAgentResultPage(queryId, params)
        : nl2sqlApi.getResultPage(queryId, params);

      fetcher.then((pageRes) => {
        if (!mountedRef.current) return;
        setTurns((prev) => prev.map((turn) => turn.id === item.id ? {
          ...turn,
          resultSource: source,
          result: {
            columns: pageRes.columns ?? turn.result?.columns ?? [],
            rows: pageRes.rows ?? [],
            row_count: pageRes.totalRows ?? pageRes.rows?.length ?? turn.result?.row_count ?? 0,
            total_rows: pageRes.totalRows ?? pageRes.rows?.length ?? turn.result?.total_rows ?? 0,
            has_more: pageRes.hasMore ?? false,
            limit: pageRes.perPage ?? params.per_page,
            offset: ((pageRes.page ?? params.page) - 1) * (pageRes.perPage ?? params.per_page),
            execution_time_ms: turn.result?.execution_time_ms,
          },
        } : turn));
      }).catch(() => {
        // Historical result snapshots may expire. Keep the SQL visible and let users rerun explicitly.
      });
    });
  }, []);

  // ── P3-1: Clarification handler
  const handleClarify = async (selectedOption?: ClarificationContextType['options'][number], freeText?: string) => {
    if (clarifyLoading) return;
    const targetTurnId = effectiveClarifyingTurnId ?? clarifyingTurnId;
    if (!clarificationContext || !targetTurnId) return;

    const question = clarificationContext.original_question;
    const currentClarifyContext = clarificationContext;
    const sourceClarifyingTurnId = targetTurnId;
    const effectiveConversationId = conversationId ?? `clarify-${Date.now()}`;
    const routeMetaFromTurn = turns.find((turn) => turn.id === sourceClarifyingTurnId);
    const idSuffix = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const answerTurnId = `clarify-answer-${idSuffix}`;
    const resultTurnId = `clarify-result-${idSuffix}`;
    const selectedAnswer = selectedOption
      ? t('nl2sql.clarification.selectedOptionAnswer', {
          table: selectedOption.table_name,
          column: selectedOption.column_name,
        })
      : '';
    const answerText = freeText?.trim() || selectedAnswer;

    setClarifyLoading(true);
    setClarifyingTurnId(resultTurnId);
    setClarificationPausedByUser(false);
    setTurns((prev) => [
      ...prev,
      {
        id: answerTurnId,
        role: 'user',
        question: answerText,
      },
      {
        id: resultTurnId,
        role: 'assistant',
        question,
        isGenerating: true,
        dataSourceId: routeMetaFromTurn?.dataSourceId,
        routeConfidence: routeMetaFromTurn?.routeConfidence ?? routeMetaRef.current?.route_confidence ?? null,
        routingMethod: routeMetaFromTurn?.routingMethod ?? routeMetaRef.current?.routing_method ?? null,
        semanticContext: routeMetaFromTurn?.semanticContext ?? routeMetaRef.current?.semantic_context ?? null,
        clarifyStage: 'queued',
        clarifyStageMessage: t('nl2sql.clarification.queued'),
      },
    ]);
    setTimeout(scrollToBottom, 50);
    try {
      const start = await nl2sqlApi.clarifyAsync({
        session_id: sessionId,
        conversation_id: effectiveConversationId,
        clarification_context: {
          ...currentClarifyContext,
          conversation_id: effectiveConversationId,
          turn: currentClarifyContext.turn + 1,
        },
        selected_option: selectedOption ? { option_index: selectedOption.option_index } : undefined,
        free_text: freeText,
        route_confidence: routeMetaFromTurn?.routeConfidence ?? routeMetaRef.current?.route_confidence,
        routing_method: routeMetaFromTurn?.routingMethod ?? routeMetaRef.current?.routing_method ?? undefined,
        semantic_context: routeMetaFromTurn?.semanticContext ?? routeMetaRef.current?.semantic_context ?? undefined,
        source_query_task_id: routeMetaFromTurn?.queryTaskId ?? undefined,
      });
      const clarifyTaskId = start.taskId;
      setTurns(prev => prev.map(turn => turn.id === resultTurnId ? {
        ...turn,
        clarifyTaskId,
        clarifyStage: 'queued',
        clarifyStageMessage: t('nl2sql.clarification.queued'),
      } : turn));

      const res = await new Promise<import('@/types').ClarifyResponse>((resolve, reject) => {
        const cleanup = () => {
          const unsub = clarifyTaskUnsubMapRef.current[clarifyTaskId];
          if (unsub) {
            try { unsub(); } catch { /* noop */ }
            delete clarifyTaskUnsubMapRef.current[clarifyTaskId];
          }
        };
        const unsub = streamNl2sqlClarifyTask(clarifyTaskId, {
          onEvent: (evt) => {
            if (!mountedRef.current) return;
            setTurns(prev => prev.map(t => t.id === resultTurnId ? {
              ...t,
              clarifyTaskId: evt.task_id,
              clarifyStage: evt.stage ?? t.clarifyStage ?? null,
              clarifyStageMessage: evt.message ?? t.clarifyStageMessage ?? null,
            } : t));
          },
          onDone: (evt) => {
            cleanup();
            const response = evt.response;
            if (!response) {
              reject(new Error(evt.error || 'clarify task ended without response'));
              return;
            }
            resolve(response);
          },
          onError: (err) => {
            cleanup();
            reject(new Error(err));
          },
        });
        clarifyTaskUnsubMapRef.current[clarifyTaskId] = unsub;
      });

      if (res.error) {
        setTurns(prev => prev.map(turn => turn.id === resultTurnId ? {
          ...turn,
          error: res.error ?? t('nl2sql.clarifyFailed'),
          isGenerating: false,
          clarifyTaskId: null,
          clarifyStage: null,
          clarifyStageMessage: null,
        } : turn));
        message.error(res.error);
        return;
      }

      // Handle chained clarification
      if (res.pending_clarification) {
        setClarificationPausedByUser(false);
        setClarificationContext(res.pending_clarification);
        setClarifyingTurnId(resultTurnId);
        setTurns(prev => prev.map(turn => turn.id === resultTurnId ? {
          ...turn,
          clarificationQuestion: res.pending_clarification?.clarification_question ?? turn.clarificationQuestion,
          clarificationTurn: res.pending_clarification?.turn ?? turn.clarificationTurn,
          clarificationConfirmedRequirements: res.pending_clarification?.confirmed_requirements ?? turn.clarificationConfirmedRequirements,
          clarificationMissingRequirements: res.pending_clarification?.missing_requirements ?? turn.clarificationMissingRequirements,
          error: null,
          isGenerating: false,
          clarifyTaskId: null,
          clarifyStage: null,
          clarifyStageMessage: null,
        } : turn));
        setTimeout(scrollToBottom, 50);
        return;
      }

      // Defensive fallback:
      // some backend/model paths may still return clarification context under
      // data.clarification_context with empty SQL. Keep clarification UI open.
      const normalizedSql = (res.data?.sql ?? '').trim();
      const fallbackClarifyCtx = res.data?.clarification_context ?? null;
      if (!normalizedSql && fallbackClarifyCtx?.clarification_question) {
        setClarificationPausedByUser(false);
        setClarificationContext(fallbackClarifyCtx);
        setClarifyingTurnId(resultTurnId);
        setTurns(prev => prev.map(turn => turn.id === resultTurnId ? {
          ...turn,
          clarificationQuestion: fallbackClarifyCtx.clarification_question ?? turn.clarificationQuestion,
          clarificationTurn: fallbackClarifyCtx.turn ?? turn.clarificationTurn,
          clarificationConfirmedRequirements: fallbackClarifyCtx.confirmed_requirements ?? turn.clarificationConfirmedRequirements,
          clarificationMissingRequirements: fallbackClarifyCtx.missing_requirements ?? turn.clarificationMissingRequirements,
          error: null,
          isGenerating: false,
          clarifyTaskId: null,
          clarifyStage: null,
          clarifyStageMessage: null,
        } : turn));
        setTimeout(scrollToBottom, 50);
        return;
      }

      // Clarify API can return data.error without pending_clarification;
      // keep the clarification card open so users can continue补充 or retry.
      if (res.data?.error) {
        setTurns(prev => prev.map(t => t.id === resultTurnId ? {
          ...t,
          isGenerating: false,
          error: res.data?.error ?? t.error,
          clarifyTaskId: null,
          clarifyStage: null,
          clarifyStageMessage: null,
        } : t));
        message.error(res.data.error);
        return;
      }

      if (res.data) {
        setClarificationContext(null);
        setClarifyingTurnId(null);
        setClarificationPausedByUser(false);
        const { query_id } = res.data;
        const sql = normalizedSql;
        const clarifiedDataSourceId =
          res.data.data_source_id
          || selectedOption?.data_source_id
          || currentClarifyContext.options[0]?.data_source_id
          || null;
        if (clarifiedDataSourceId && selectedDataSourceId !== clarifiedDataSourceId) {
          setSelectedDataSourceId(clarifiedDataSourceId);
          setSuggestedSource(null);
        }

        // F-04: Use functional update to avoid stale closure
        setTurns(prev => prev.map(t => t.id === resultTurnId ? {
          ...t,
          dataSourceId: clarifiedDataSourceId ?? t.dataSourceId ?? undefined,
          sql: sql ?? t.sql,
          isGenerating: false as const,
          queryId: query_id,
          routeConfidence: t.routeConfidence ?? routeMetaRef.current?.route_confidence ?? null,
          routingMethod: t.routingMethod ?? routeMetaRef.current?.routing_method ?? null,
          semanticContext: t.semanticContext ?? routeMetaRef.current?.semantic_context ?? null,
          appliedRules: mergeAppliedRules(t.appliedRules, res.data?.applied_rules),
          clarificationFallbackMode: res.data?.fallback_mode ?? null,
          clarificationQuestion: sql ? null : t.clarificationQuestion,
          error: null,
          clarifyTaskId: null,
          clarifyStage: null,
          clarifyStageMessage: null,
        } : t));

        if (!conversationId) {
          setConversationId(effectiveConversationId);
        }

        // F-01: Resolve datasource from clarification options (dsId is not in scope here).
        // F-02: Guard setTurns against unmounted component.
        const quDsId = clarifiedDataSourceId ?? currentClarifyContext.options[0]?.data_source_id ?? '';
        if (quDsId) {
          nl2sqlApi.queryUnderstanding(quDsId, currentClarifyContext.original_question).then((qu) => {
            if (mountedRef.current) {
              setTurns(prev => prev.map(t => t.id === resultTurnId ? {
                ...t,
                queryUnderstanding: qu,
              } : t));
            }
          }).catch(() => {});
        }

        setTimeout(scrollToBottom, 50);
      } else {
        message.warning(t('nl2sql.clarifyFailed'));
      }
    } catch (err) {
      message.error(`${t('nl2sql.clarifyFailed')}: ${(err as Error).message}`);
      setTurns(prev => prev.map(t => t.id === resultTurnId ? {
        ...t,
        isGenerating: false,
        error: (err as Error).message,
        clarifyTaskId: null,
        clarifyStage: null,
        clarifyStageMessage: null,
      } : t));
    } finally {
      setClarifyLoading(false);
    }
  };

  const handleAutoMultiSource = useCallback(async (question: string) => {
    if (embeddingRequiredBlocked) {
      message.warning(t('nl2sql.embeddingRequiredForExplore'));
      return;
    }

    const turnId = `agent-${Date.now()}`;
    const effectiveConversationId =
      conversationId ?? `multi-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    if (!conversationId) {
      setConversationId(effectiveConversationId);
    }
    const initialStage = 'request_validation';
    const initialMessage = t('nl2sql.routeMessages.requestValidationStart');

    setTurns((prev) => [
      ...prev,
      {
        id: turnId,
        role: 'assistant',
        question,
        isGenerating: true,
        queryStage: initialStage,
        queryStageMessage: initialMessage,
        queryElapsedMs: 0,
        queryStageTimeline: createMultiSourceProgressTimeline(0),
      },
    ]);
    setTimeout(scrollToBottom, 50);

    try {
      const start = await nl2sqlApi.agentExecuteAsync({
        question,
        conversation_id: effectiveConversationId,
      });
      const taskId = start.taskId;
      setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
        ...turn,
        queryTaskId: taskId,
      } : turn));

      const finalEvent = await new Promise<import('@/types').AgentTaskEvent>((resolve, reject) => {
        const cleanup = () => {
          const unsub = agentTaskUnsubMapRef.current[taskId];
          if (unsub) {
            try { unsub(); } catch { /* noop */ }
            delete agentTaskUnsubMapRef.current[taskId];
          }
        };

        const unsub = streamNl2sqlAgentTask(taskId, {
          onEvent: (evt) => {
            if (!mountedRef.current) return;
            setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
              ...(turn.queryStage === evt.stage || !evt.stage
                ? {}
                : {
                    queryStageHistory: [
                      ...(turn.queryStageHistory ?? []),
                      evt.stage,
                    ],
                  }),
              ...turn,
              queryTaskId: evt.task_id,
              queryStage: evt.stage ?? turn.queryStage ?? null,
              queryStageMessage: evt.message ?? turn.queryStageMessage ?? null,
              queryElapsedMs: evt.elapsed_ms ?? turn.queryElapsedMs ?? 0,
              queryStageTimeline: evt.stage
                ? appendOrUpdateStageTimeline(
                    turn.queryStageTimeline,
                    evt.stage,
                    evt.message ?? null,
                    evt.elapsed_ms ?? 0,
                    evt.stage_elapsed_ms ?? null,
                  )
                : (turn.queryStageTimeline ?? []),
            } : turn));

            if (evt.status === 'completed' || evt.status === 'failed') {
              cleanup();
              resolve(evt);
            }
          },
          onError: (err) => {
            cleanup();
            reject(new Error(err));
          },
        });
        agentTaskUnsubMapRef.current[taskId] = unsub;
      });

      if (finalEvent.error) {
        setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
          ...turn,
          isGenerating: false,
          error: finalEvent.error ?? t('nl2sql.executionError'),
        } : turn));
        message.error(finalEvent.error ?? t('nl2sql.executionError'));
        return;
      }

      const res = finalEvent.response;
      if (!res) {
        const fallbackError = t('nl2sql.executionError');
        setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
          ...turn,
          isGenerating: false,
          error: fallbackError,
        } : turn));
        message.error(fallbackError);
        return;
      }

      if (res.error) {
        const err = res.error ?? t('nl2sql.executionError');
        const totalExecMs =
          (res as unknown as { total_execution_ms?: number; totalExecutionMs?: number }).total_execution_ms
          ?? (res as unknown as { total_execution_ms?: number; totalExecutionMs?: number }).totalExecutionMs
          ?? null;
        setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
          ...turn,
          isGenerating: false,
          error: err,
          usedReferences: res.usedReferences,
          multiSourceSteps: (res.steps ?? []) as AgentStepResult[],
          multiSourceTotalExecutionMs: totalExecMs,
          queryStageTimeline: appendMultiSourceStepTimeline(
            turn.queryStageTimeline,
            (res.steps ?? []) as AgentStepResult[],
            turn.queryElapsedMs ?? 0,
            t,
          ),
        } : turn));
        message.error(err);
        return;
      }

      const steps = (res.steps ?? []) as AgentStepResult[];
      const totalExecMs =
        (res as unknown as { total_execution_ms?: number; totalExecutionMs?: number }).total_execution_ms
        ?? (res as unknown as { total_execution_ms?: number; totalExecutionMs?: number }).totalExecutionMs
        ?? null;
      if (steps.some((step) => step.error)) {
        const err = steps.find((step) => step.error)?.error ?? t('nl2sql.executionError');
        setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
          ...turn,
          isGenerating: false,
          error: err,
          usedReferences: res.usedReferences,
          multiSourceSteps: steps,
          multiSourceTotalExecutionMs: totalExecMs,
          queryStageTimeline: appendMultiSourceStepTimeline(
            turn.queryStageTimeline,
            steps,
            turn.queryElapsedMs ?? 0,
            t,
          ),
        } : turn));
        message.error(err);
        return;
      }

      const mergedResult = resolveMultiSourceResult(res, steps);
      const responseConversationId =
        (res as unknown as { conversationId?: string; conversation_id?: string }).conversationId
        ?? (res as unknown as { conversationId?: string; conversation_id?: string }).conversation_id
        ?? effectiveConversationId;
      const responseQueryId =
        (res as unknown as { queryId?: string; query_id?: string }).queryId
        ?? (res as unknown as { queryId?: string; query_id?: string }).query_id
        ?? undefined;
      setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
        ...turn,
        isGenerating: false,
        queryId: responseQueryId,
        explanation: t('nl2sql.autoMultiSourceExplanation'),
        usedReferences: res.usedReferences,
        multiSourceSteps: steps,
        multiSourceTotalExecutionMs: totalExecMs,
        queryStageTimeline: appendMultiSourceStepTimeline(
          turn.queryStageTimeline,
          steps,
            turn.queryElapsedMs ?? 0,
            t,
          ),
        result: mergedResult,
        resultSource: 'agent',
      } : turn));
      setConversationId(responseConversationId);
      setTurnViewTab(turnId, 'table');
      message.success(t('nl2sql.autoMultiSourceDone'));
    } catch (err) {
      const msg = (err as Error).message;
      setTurns((prev) => prev.map((turn) => turn.id === turnId ? {
        ...turn,
        isGenerating: false,
        error: msg,
      } : turn));
      message.error(`${t('nl2sql.executionError')}: ${msg}`);
      setTimeout(scrollToBottom, 50);
    }
  }, [conversationId, createMultiSourceProgressTimeline, embeddingRequiredBlocked, setTurnViewTab, t]);

  // ── Save view
  const handleSaveView = useCallback(async (turnId: string) => {
    const turn = turns.find(t => t.id === turnId);
    if (!turn?.sql || !turn.question || !turn.dataSourceId) return;

    // Find the query_id from the turn (set by the API response during generation)
    const queryId = resolveTurnQueryId(turn);
    if (!queryId) {
      message.error(t('nl2sql.saveViewFailed') + ': missing query id');
      return;
    }

    try {
      await nl2sqlApi.saveView({
        query_id: queryId,
        name: `${turn.question.slice(0, 40)}…`,
        description: turn.question,
        conversation_id: conversationId ?? undefined,
      });
      await refetchSavedViews();
      message.success(t('nl2sql.viewSaved'));
    } catch {
      message.error(t('nl2sql.saveViewFailed'));
    }
  }, [turns, conversationId, t, refetchSavedViews]);

  const ensureWritableSqlKnowledgeSpace = useCallback(async (dataSourceId: string): Promise<Nl2sqlReferencePack> => {
    const spaces = await nl2sqlApi.listSqlKnowledgeSpaces({
      datasourceId: dataSourceId,
      includeGlobal: false,
    });
    const enabledBoundSpace = spaces.find((space) =>
      space.enabled
      && (
        space.datasourceId === dataSourceId
        || (space.datasourceBindings ?? []).includes(dataSourceId)
      )
    );
    if (enabledBoundSpace) return enabledBoundSpace;

    return nl2sqlApi.createSqlKnowledgeSpace({
      name: t('nl2sql.defaultKnowledgeSpaceName'),
      description: t('nl2sql.defaultKnowledgeSpaceDesc'),
      datasourceIds: [dataSourceId],
      verified: true,
      tags: ['data-exploration', 'executed-sql'],
    });
  }, [t]);

  const handleSaveSqlToKnowledge = useCallback(async (turnId: string) => {
    const turn = turns.find((item) => item.id === turnId);
    const sql = (turn?.editedSql ?? turn?.sql ?? '').trim();
    if (!turn || !sql) return;
    if (!turn.result) {
      message.warning(t('nl2sql.saveKnowledgeNeedsResult'));
      return;
    }
    if (!embeddingAvailable) {
      message.warning(t('nl2sql.embeddingRequiredForKnowledgeSave'));
      return;
    }
    const dataSourceId = turn.dataSourceId ?? selectedDataSourceId;
    if (!dataSourceId) {
      message.error(t('nl2sql.selectDataSourceFirst'));
      return;
    }
    const queryId = resolveTurnQueryId(turn);
    setSavingKnowledgeTurnIds((prev) => ({ ...prev, [turnId]: true }));
    try {
      const space = await ensureWritableSqlKnowledgeSpace(dataSourceId);
      const content = buildSqlKnowledgeFileContent({
        question: turn.question,
        sql,
        dataSourceId,
        queryId,
        explanation: turn.explanation,
        result: turn.result,
      });
      const filename = safeSqlKnowledgeFilename(turn.question, queryId);
      const file = new File([content], filename, { type: 'text/plain;charset=utf-8' });
      await nl2sqlApi.uploadSqlKnowledgeFiles(space.id, [file]);
      await Promise.all([
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.referencePacks(dataSourceId) }),
        qc.invalidateQueries({ queryKey: queryKeys.nl2sql.sqlKnowledge.all() }),
      ]);
      message.success(t('nl2sql.savedToKnowledge', { name: space.name }));
    } catch (err) {
      message.error(err instanceof Error ? err.message : t('nl2sql.saveKnowledgeFailed'));
    } finally {
      setSavingKnowledgeTurnIds((prev) => {
        const next = { ...prev };
        delete next[turnId];
        return next;
      });
    }
  }, [
    embeddingAvailable,
    ensureWritableSqlKnowledgeSpace,
    qc,
    selectedDataSourceId,
    t,
    turns,
  ]);

  // ── Edit SQL
  const handleEditSql = useCallback((turnId: string, sql: string) => {
    setTurns(prev => prev.map(t => t.id === turnId ? { ...t, editedSql: sql } : t));
    setSqlEditingId(null);
  }, []);

  // ── Load from saved view
  const handleLoadView = useCallback((view: EditableView) => {
    if (view.conversation_id) {
      setPendingViewConversationId(view.conversation_id);
      setSavedViewsDrawerOpen(false);
      return;
    }
    const resolvedQueryId = String(view.query_id || view.id || '').trim();
    const resolvedDataSourceId = view.data_source_id ?? selectedDataSourceId ?? null;
    setClarificationContext(null);
    setClarifyingTurnId(null);
    setClarificationPausedByUser(false);
    setSelectedDataSourceId(resolvedDataSourceId);
    setSavedViewsDrawerOpen(false);
    setActiveConversationId(null);
    setConversationPage(1);
    setConversationHasMore(false);
    setTurns([{
      id: `view-${Date.now()}`,
      role: 'user',
      question: view.question,
      dataSourceId: resolvedDataSourceId ?? undefined,
    }, {
      id: `view-sql-${Date.now()}`,
      role: 'assistant',
      question: view.question,
      sql: view.sql,
      // Carry the original query_id so "Execute" updates the right row.
      queryId: resolvedQueryId,
      dataSourceId: resolvedDataSourceId ?? undefined,
      resultSource: resolvedDataSourceId ? 'query' : 'agent',
    }]);
    setTimeout(scrollToBottom, 50);
  }, [selectedDataSourceId]);

  // ── P3-2: Load conversation thread
  const handleLoadConversation = useCallback(async (conversationId: string) => {
    setConversationDrawerOpen(false);
    try {
      const conv = await nl2sqlApi.getConversation(conversationId, {
        page: 1,
        per_page: CONVERSATION_PAGE_SIZE,
      });
      setClarificationContext(null);
      setClarifyingTurnId(null);
      setClarificationPausedByUser(false);
      const loadedTurns = mapConversationMessagesToTurns(conv.messages);
      setTurns(loadedTurns);
      hydrateHistoricalResultPages(loadedTurns);
      setConversationPage(1);
      setConversationHasMore(conv.has_more);
      setActiveConversationId(conversationId);
      setConversationId(conversationId);
      // Conversation-mode clarification recovery:
      // clarification session snapshots are stored under conversation id,
      // so re-opened historical conversations must rehydrate from that key.
      try {
        const clarify = await nl2sqlApi.getClarify(conversationId);
        const pending = clarify?.pending_clarification;
        if (pending) {
          const latestClarifyAssistantTurn = [...loadedTurns]
            .reverse()
            .find((turn) => turn.role === 'assistant' && !!turn.clarificationQuestion);
          const targetTurnId = latestClarifyAssistantTurn?.id ?? `conv-clarify-active-${Date.now()}`;

          if (!latestClarifyAssistantTurn) {
            const syntheticTurn: NlTurn = {
              id: targetTurnId,
              role: 'assistant',
              question: pending.original_question,
              clarificationQuestion: pending.clarification_question,
              clarificationTurn: pending.turn ?? null,
              clarificationConfirmedRequirements: pending.confirmed_requirements ?? null,
              clarificationMissingRequirements: pending.missing_requirements ?? null,
              isGenerating: false,
            };
            setTurns((prev) => [...prev, syntheticTurn]);
          } else {
            setTurns((prev) => prev.map((turn) => (
              turn.id === targetTurnId
                ? {
                  ...turn,
                  clarificationQuestion: turn.clarificationQuestion ?? pending.clarification_question,
                  clarificationTurn: turn.clarificationTurn ?? (pending.turn ?? null),
                  clarificationConfirmedRequirements:
                    turn.clarificationConfirmedRequirements ?? (pending.confirmed_requirements ?? null),
                  clarificationMissingRequirements:
                    turn.clarificationMissingRequirements ?? (pending.missing_requirements ?? null),
                }
                : turn
            )));
          }

          setClarificationContext(pending);
          setClarifyingTurnId(targetTurnId);
        }
      } catch {
        // Ignore — conversation may not have pending clarification.
      }
      setTimeout(scrollToBottom, 50);
    } catch {
      message.error(t('nl2sql.loadConversationFailed'));
    }
  }, [hydrateHistoricalResultPages, t]);

  const loadOlderConversationMessages = useCallback(async () => {
    if (!activeConversationId || !conversationHasMore || conversationLoadingMore) return;
    const container = messagesContainerRef.current;
    if (!container) return;

    const previousScrollHeight = container.scrollHeight;
    const previousScrollTop = container.scrollTop;
    const nextPage = conversationPage + 1;

    setConversationLoadingMore(true);
    try {
      const conv = await nl2sqlApi.getConversation(activeConversationId, {
        page: nextPage,
        per_page: CONVERSATION_PAGE_SIZE,
      });
      const olderTurns = mapConversationMessagesToTurns(conv.messages);
      if (olderTurns.length > 0) {
        setTurns(prev => [...olderTurns, ...prev]);
        hydrateHistoricalResultPages(olderTurns);
      }
      setConversationPage(nextPage);
      setConversationHasMore(conv.has_more);
      setTimeout(() => {
        const el = messagesContainerRef.current;
        if (!el) return;
        const newScrollHeight = el.scrollHeight;
        el.scrollTop = newScrollHeight - previousScrollHeight + previousScrollTop;
      }, 0);
    } catch {
      message.error(t('nl2sql.loadConversationFailed'));
    } finally {
      setConversationLoadingMore(false);
    }
  }, [
    activeConversationId,
    conversationHasMore,
    conversationLoadingMore,
    conversationPage,
    hydrateHistoricalResultPages,
    t,
  ]);

  const handleMessagesScroll = useCallback(() => {
    const el = messagesContainerRef.current;
    if (!el) return;
    if (el.scrollTop <= 80) {
      void loadOlderConversationMessages();
    }
  }, [loadOlderConversationMessages]);

  useEffect(() => {
    if (!pendingViewConversationId) return;
    void handleLoadConversation(pendingViewConversationId);
    setPendingViewConversationId(null);
  }, [pendingViewConversationId, handleLoadConversation]);

  const slashInput = input.trimStart();
  const showSlashMenu = slashInput.startsWith('/');
  const filteredSlashCommands = useMemo(() => {
    const keyword = slashInput.slice(1).toLowerCase();
    if (!keyword) return SLASH_COMMANDS;
    return SLASH_COMMANDS.filter((cmd) => cmd.key.startsWith(keyword) || cmd.label.slice(1).startsWith(keyword));
  }, [slashInput, SLASH_COMMANDS]);

  useEffect(() => {
    setSlashActiveIndex(0);
  }, [slashInput]);

  const applySlashCommand = (command: 'multi') => {
    if (command === 'multi') {
      setInput('/multi ');
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  };

  const applySlashCommandByIndex = (index: number) => {
    const cmd = filteredSlashCommands[index];
    if (!cmd) return;
    applySlashCommand(cmd.key);
  };

  // ── Keyboard
  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (showSlashMenu && filteredSlashCommands.length > 0) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setSlashActiveIndex((prev) => (prev + 1) % filteredSlashCommands.length);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setSlashActiveIndex((prev) => (prev - 1 + filteredSlashCommands.length) % filteredSlashCommands.length);
        return;
      }
      if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
        e.preventDefault();
        applySlashCommandByIndex(slashActiveIndex);
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        setInput('');
        return;
      }
    }

    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      handleSend();
    }
  };

  const defaultModeText = (() => {
    if (selectedDataSourceId) return t('nl2sql.defaultModePinnedSingle');
    if (dataSources.length === 1) return t('nl2sql.defaultModeSingleFallback');
    return t('nl2sql.defaultModeAutoRoute');
  })();
  const modeStatusText = (() => {
    if (selectedDataSourceId) {
      const ds = dataSources.find((d) => d.id === selectedDataSourceId);
      return t('nl2sql.modeStatus.single', { name: ds?.name ?? selectedDataSourceId });
    }
    if (autoRouting) {
      if (suggestedSource) {
        return t('nl2sql.modeStatus.autoCandidate', {
          name: suggestedSource.name,
          confidence: Math.round(suggestedSource.confidence * 100),
        });
      }
      if (isSuggestingSource && (routingStage === 'search_candidates' || routingStage === 'ai_confirming')) {
        return t('nl2sql.modeStatus.autoDetecting');
      }
      return t('nl2sql.modeStatus.autoIdle');
    }
    return t('nl2sql.modeStatus.manual');
  })();
  const feedbackReasonOptions = [
    { key: 'sql_wrong', label: t('nl2sql.feedback.reasonSqlWrong') },
    { key: 'result_wrong', label: t('nl2sql.feedback.reasonResultWrong') },
    { key: 'need_clarification', label: t('nl2sql.feedback.reasonNeedClarification') },
    { key: 'too_slow', label: t('nl2sql.feedback.reasonTooSlow') },
    { key: 'noisy', label: t('nl2sql.feedback.reasonNoisy') },
  ] as const;
  const feedbackTypeFromReason = (reasonKey: string): 'thumbs_down' | 'clarification_needed' => {
    if (reasonKey === 'need_clarification') return 'clarification_needed';
    return 'thumbs_down';
  };

  // ── Clear conversation
  const handleClear = () => {
    if (turns.length > 0) {
      Modal.confirm({
        title: t('nl2sql.confirmClear'),
        content: t('nl2sql.confirmClearContent'),
        okText: t('common.confirm'),
        cancelText: t('common.cancel'),
        okButtonProps: { style: { color: '#fff' } },
        onOk: () => {
          setTurns([]);
          setConversationId(null);
          setActiveConversationId(null);
          setConversationPage(1);
          setConversationHasMore(false);
          clearPromptQueue();
          setClarificationContext(null);
          setClarifyingTurnId(null);
          setClarificationPausedByUser(false);
        },
      });
    } else {
      setTurns([]);
      setConversationId(null);
      setActiveConversationId(null);
      setConversationPage(1);
      setConversationHasMore(false);
      clearPromptQueue();
      setClarificationContext(null);
      setClarifyingTurnId(null);
      setClarificationPausedByUser(false);
    }
  };

  // ─── Render ───────────────────────────────────────────────────────────
  const leftPanelWidth = leftPanelCollapsed ? 52 : 200;
  const rightRailWidth = 44;

  return (
    <ErrorBoundary>
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
      <div style={{ flex: 1, minHeight: 0, overflow: 'hidden' }}>
    <Layout style={{ height: '100%', overflow: 'hidden', display: 'flex', flexDirection: 'row' }}>
      {/* ── Left: Data source picker + schema ── */}
      <div style={{
        width: leftPanelWidth,
        borderRight: '1px solid var(--border-subtle)',
        background: 'var(--bg-surface)',
        display: 'flex', flexDirection: 'column',
        overflow: 'hidden', flexShrink: 0,
        transition: 'width 0.2s ease',
      }}>
        {/* Header */}
        <div style={{ padding: '10px 12px', borderBottom: '1px solid var(--border-subtle)', display: 'flex', alignItems: 'center', gap: 8 }}>
          <Text strong style={{ fontSize: 13, color: 'var(--text-primary)', flex: 1 }}>
            {leftPanelCollapsed ? '' : t('nl2sql.dataSources')}
          </Text>
          <Tooltip title={leftPanelCollapsed ? t('nl2sql.expand') : t('nl2sql.collapse')}>
            <Button
              type="text" size="small"
              icon={leftPanelCollapsed ? <RightOutlined /> : <LeftOutlined />}
              onClick={() => setLeftPanelCollapsed(!leftPanelCollapsed)}
              style={{ color: 'var(--text-muted)', padding: '0 4px', height: 24, width: 24, minWidth: 24 }}
            />
          </Tooltip>
        </div>

        {!leftPanelCollapsed && (
          <>
            {/* Source list */}
            <div style={{ flex: 1, overflow: 'auto', padding: '8px' }}>
              {dsLoading ? (
                <div style={{ textAlign: 'center', padding: 24 }}>
                  <Spin size="small" />
                </div>
              ) : (
                <DataSourcePicker
                  dataSources={dataSources}
                  selectedId={selectedDataSourceId}
                  onSelect={(id) => setSelectedDataSourceId(id)}
                  t={t}
                />
              )}
            </div>

            {/* Bottom actions */}
            <div style={{ padding: '8px', borderTop: '1px solid var(--border-subtle)', display: 'flex', flexDirection: 'column', gap: 4 }}>
              <Button
                icon={<StarOutlined />}
                onClick={async () => {
                  setSavedViewsDrawerOpen(true);
                  await refetchSavedViews();
                }}
                style={{ fontSize: 12, borderRadius: 6 }}
                block
              >
                {t('nl2sql.savedViews')}
              </Button>
              <Button
                icon={<CommentOutlined />}
                onClick={() => setConversationDrawerOpen(true)}
                style={{ fontSize: 12, borderRadius: 6 }}
                block
              >
                {t('nl2sql.conversations')}
              </Button>
            </div>
          </>
        )}

      </div>

      {/* ── Center: Chat + results ── */}
      <div style={{
        flex: 1, display: 'flex', flexDirection: 'column',
        background: 'var(--bg-surface)', height: '100%', overflow: 'hidden', minWidth: 0,
      }}>
        {/* Top bar */}
        <div style={{
          padding: '8px 16px', borderBottom: '1px solid var(--border-subtle)',
          display: 'flex', alignItems: 'center', gap: 10,
          flexShrink: 0, flexWrap: 'wrap',
        }}>
          <Text style={{ fontSize: 14, color: 'var(--text-primary)', fontWeight: 600 }}>
            NL2SQL
          </Text>
          {selectedDataSource && (
            <Tag color="purple" style={{ fontSize: 11 }}>
              <DatabaseOutlined style={{ marginRight: 4 }} />
              {selectedDataSource.name}
            </Tag>
          )}
          {turns.length > 0 && (
            <Tag style={{ fontSize: 11, color: 'var(--text-muted)' }}>
              {turns.length} {t('nl2sql.turns', { count: turns.length })}
            </Tag>
          )}
          <div style={{ marginLeft: 'auto', display: 'flex', gap: 8 }}>
            {turns.length > 0 && (
              <Button size="small" onClick={handleClear} style={{ fontSize: 11 }}>
                {t('nl2sql.newConversation')}
              </Button>
            )}
          </div>
        </div>

        {/* The built-in model is usable; a remote API is an optional quality upgrade. */}
        {embeddingConfig?.configured_via === 'local' && localEmbeddingNotice.visible && (
          <Alert
            type="info"
            showIcon
            closable
            onClose={localEmbeddingNotice.dismiss}
            message={t('apikeys.noKeyWarning')}
            description={t('apikeys.noKeyWarningDesc')}
            action={
              <Button
                size="small"
                type="primary"
                onClick={() => navigate('/keys')}
              >
                {t('apikeys.configureEmbeddingEnhancement')}
              </Button>
            }
            style={{ margin: '8px 16px' }}
          />
        )}

        {/* Cold start warning: no indexed columns */}
        {selectedDataSourceId && semanticsData && semanticsData.columns.filter(c => c.is_indexed).length === 0 && (
          <Alert
            type="warning"
            showIcon
            message={t('nl2sql.coldStartWarning')}
            style={{ margin: '8px 16px 0', borderRadius: 6 }}
            closable
          />
        )}

        {/* Messages */}
        <div
          ref={messagesContainerRef}
          onScroll={handleMessagesScroll}
          style={{
          flex: 1, overflow: 'auto', padding: '16px 14px',
          display: 'flex', flexDirection: 'column', gap: 16,
        }}>
          {conversationLoadingMore && (
            <div style={{ textAlign: 'center', padding: '4px 0' }}>
              <Text style={{ fontSize: 12, color: 'var(--text-muted)' }}>{t('common.loading')}</Text>
            </div>
          )}
          {turns.length === 0 && (
            <div style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
              <div style={{ textAlign: 'center', maxWidth: 480 }}>
                {dataSources.length === 0 ? (
                  // First-run experience: nothing to query against. Send
                  // users straight to the Data Sources page instead of
                  // letting them stare at an empty input.
                  <>
                    <div style={{ fontSize: 48, marginBottom: 16 }}>🗄️</div>
                    <Title level={4} style={{ marginBottom: 8 }}>
                      {t('nl2sql.noDataSourcesTitle')}
                    </Title>
                    <Text type="secondary" style={{ fontSize: 13, display: 'block', marginBottom: 20 }}>
                      {t('nl2sql.noDataSourcesDesc')}
                    </Text>
                    <Button
                      type="primary"
                      icon={<PlusOutlined />}
                      onClick={() => navigate('/datasources')}
                    >
                      {t('nl2sql.goToDataSources')}
                    </Button>
                  </>
                ) : (
                  <>
                    <div style={{ fontSize: 48, marginBottom: 16 }}>📊</div>
                    <Title level={4} style={{ marginBottom: 8 }}>{t('nl2sql.welcomeTitle')}</Title>
                    <Text type="secondary" style={{ fontSize: 13, display: 'block', marginBottom: 20 }}>
                      {t('nl2sql.welcomeDesc')}
                    </Text>

                    {/* Quick templates */}
                    <div style={{
                      background: 'var(--bg-elevated)', borderRadius: 12,
                      padding: '14px 16px', border: '1px solid var(--border-default)',
                      textAlign: 'left', marginBottom: 16,
                    }}>
                      <Text style={{ fontSize: 12, color: 'var(--text-muted)', display: 'block', marginBottom: 10 }}>
                        {t('nl2sql.quickQuestions')}
                      </Text>
                      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                        {templateQuestions.map((tmpl) => (
                          <Button
                            key={tmpl.key}
                            type="text"
                            size="small"
                            onClick={() => {
                              // With dynamic routing, any template fills the input
                              // even without a picked source — the Send handler
                              // will pick one at submit time.
                              setInput(tmpl.question);
                              inputRef.current?.focus();
                            }}
                            style={{
                              fontSize: 12, color: 'var(--text-secondary)',
                              textAlign: 'left', height: 'auto',
                              padding: '4px 8px', borderRadius: 6,
                              whiteSpace: 'normal',
                            }}
                          >
                            <span style={{ marginRight: 6 }}>{tmpl.icon}</span>
                            {tmpl.question}
                          </Button>
                        ))}
                      </div>
                    </div>

                    {!selectedDataSourceId && (
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('nl2sql.autoPickHint')}
                      </Text>
                    )}
                  </>
                )}
              </div>
            </div>
          )}

          {turns.map((turn) => (
            <div key={turn.id}>
              {/* User turn */}
              {turn.role === 'user' && turn.question && (
                <div style={{ display: 'flex', gap: 12, alignItems: 'flex-start' }}>
                  <div style={{
                    width: 32, height: 32, borderRadius: '50%',
                    background: 'var(--bubble-user-icon-bg)',
                    display: 'flex', alignItems: 'center', justifyContent: 'center',
                    color: '#fff', fontSize: 14, flexShrink: 0,
                  }}>
                    <UserOutlined />
                  </div>
                  <div style={{ flex: 1, maxWidth: '96%' }}>
                    <Text strong style={{ fontSize: 12, color: 'var(--text-secondary)', display: 'block', marginBottom: 6 }}>
                      {t('nl2sql.you')}
                    </Text>
                    <div style={{
                      background: 'var(--bubble-user-bg)',
                      border: '1px solid var(--bubble-user-border)',
                      borderRadius: 12, padding: '10px 14px',
                      color: 'var(--text-primary)', fontSize: 14, lineHeight: 1.7,
                    }}>
                      {turn.question}
                    </div>
                  </div>
                </div>
              )}

              {/* Assistant turn */}
              {turn.role === 'assistant' && (
                <div style={{ display: 'flex', gap: 12, alignItems: 'flex-start' }}>
                  <div style={{
                    width: 32, height: 32, borderRadius: '50%',
                    background: 'var(--bubble-assistant-icon-bg)',
                    display: 'flex', alignItems: 'center', justifyContent: 'center',
                    color: '#fff', fontSize: 14, flexShrink: 0,
                  }}>
                    <RobotOutlined />
                  </div>
                  <div style={{ flex: 1, maxWidth: '96%' }}>
                    <Text strong style={{ fontSize: 12, color: 'var(--text-secondary)', display: 'block', marginBottom: 6 }}>
                      NL2SQL
                    </Text>

                    {turn.clarificationFallbackMode?.endsWith('_granularity') && (
                      <Alert
                        type="warning"
                        showIcon
                        style={{ marginBottom: 10 }}
                        message={`已触发系统兜底：统计粒度默认${fallbackGranularityLabel(turn.clarificationFallbackMode)}继续生成`}
                      />
                    )}

                    {/* Multi-source execution details */}
                    {turn.multiSourceSteps && turn.multiSourceSteps.length > 0 && (
                      <MultiSourceStepsPanel
                        steps={turn.multiSourceSteps}
                        totalExecutionMs={turn.multiSourceTotalExecutionMs}
                        stageTimeline={turn.queryStageTimeline}
                        t={t}
                      />
                    )}

                    {/* Generating skeleton */}
                    {turn.isGenerating && (
                      <div style={{
                        background: 'var(--bubble-assistant-bg)',
                        border: '1px solid var(--bubble-assistant-border)',
                        borderRadius: 12, padding: '14px 16px',
                      }}>
                        <SqlCard
                          sql=""
                          isGenerating
                          stage={turn.queryStage}
                          stageMessage={turn.queryStageMessage}
                          elapsedMs={turn.queryElapsedMs}
                          stageTimeline={turn.queryStageTimeline}
                          t={t}
                        />
                      </div>
                    )}

                    {/* Clarification question */}
                    {!turn.isGenerating && turn.clarificationQuestion && !(clarificationContext && effectiveClarifyingTurnId === turn.id) && (
                      <div style={{
                        padding: '10px 14px',
                        background: 'rgba(245, 158, 11, 0.08)',
                        border: '1px solid rgba(245, 158, 11, 0.35)',
                        borderRadius: 8,
                        fontSize: 13,
                        marginBottom: 10,
                      }}>
                        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
                          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                            <QuestionCircleOutlined style={{ color: '#f59e0b' }} />
                            <Text style={{ color: '#fbbf24', fontSize: 12, fontWeight: 600 }}>
                              {turn.clarificationTurn != null && turn.clarificationTurn > 0
                                ? `Round ${turn.clarificationTurn}`
                                : t('nl2sql.clarification.needMoreInfo')}
                            </Text>
                          </div>
                        </div>
                        <div style={{ color: '#e5e7eb' }}>
                          {turn.clarificationQuestion}
                        </div>
                        {!!turn.clarificationConfirmedRequirements?.length && (
                          <div style={{ marginTop: 8, display: 'flex', flexWrap: 'wrap', gap: 6 }}>
                            {turn.clarificationConfirmedRequirements.map((item) => (
                              <Tag
                                key={`hist-confirmed-${turn.id}-${item}`}
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
                          </div>
                        )}
                        {!!turn.clarificationMissingRequirements?.length && (
                          <div style={{ marginTop: 6, display: 'flex', flexWrap: 'wrap', gap: 6 }}>
                            {turn.clarificationMissingRequirements.map((item) => (
                              <Tag
                                key={`hist-missing-${turn.id}-${item}`}
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
                          </div>
                        )}
                      </div>
                    )}

                    {/* Error */}
                    {!turn.isGenerating && turn.error && !turn.autoExecuteFailed && (
                      <div style={{
                        padding: '10px 14px', background: 'rgba(255,77,79,0.06)',
                        border: '1px solid rgba(255,77,79,0.2)', borderRadius: 8,
                        color: 'var(--color-error)', fontSize: 13, marginBottom: 10,
                      }}>
                        <ExclamationCircleOutlined style={{ marginRight: 6 }} />
                        {(() => {
                          const { type, message } = parseSqlError(turn.error ?? '');
                          const label = type === 'syntax' ? t('nl2sql.sqlError.syntax')
                            : type === 'table_not_found' ? t('nl2sql.sqlError.tableNotFound')
                            : type === 'column_not_found' ? t('nl2sql.sqlError.columnNotFound')
                            : type === 'permission' ? t('nl2sql.sqlError.permission')
                            : type === 'execution' ? t('nl2sql.sqlError.execution')
                            : t('nl2sql.executionError');
                          return <span>{label}: {message}</span>;
                        })()}
                        <Button
                          size="small" type="link"
                          onClick={() => turn.question && handleSend(turn.question)}
                          style={{ fontSize: 11, color: 'var(--color-error)', padding: '0 4px', height: 20 }}
                        >
                          {t('nl2sql.retry')}
                        </Button>
                      </div>
                    )}

                    {!turn.isGenerating && turn.error && turn.autoExecuteFailed && (
                      <Alert
                        type="warning"
                        showIcon
                        style={{ marginBottom: 10 }}
                        message={t('nl2sql.autoExecuteFailedTitle')}
                        description={turn.error}
                      />
                    )}

                    {/* SQL card */}
                    {!turn.isGenerating && turn.sql && (
                      <div style={{
                        background: 'var(--bubble-assistant-bg)',
                        border: '1px solid var(--bubble-assistant-border)',
                        borderRadius: 12, padding: '10px 14px',
                      }}>
                        <SqlCard
                          sql={turn.editedSql ?? turn.sql ?? ''}
                          explanation={turn.explanation}
                          isEditable
                          editedSql={turn.editedSql}
                          onEdit={(sql) => handleEditSql(turn.id, sql)}
                          onConfirm={() => handleExecute(turn.id)}
                          onCancel={() => setSqlEditingId(null)}
                          isGenerating={turn.isGenerating}
                          stage={turn.queryStage}
                          stageMessage={turn.queryStageMessage}
                          elapsedMs={turn.queryElapsedMs}
                          stageTimeline={turn.queryStageTimeline}
                          t={t}
                        />
                        <ExecuteBar
                          sql={turn.editedSql ?? turn.sql ?? ''}
                          canExecute={!!resolveTurnQueryId(turn)}
                          canSaveView={!!resolveTurnQueryId(turn)}
                          isExecuting={turn.isExecuting}
                          hasResult={!!turn.result}
                          isEditing={sqlEditingId === turn.id}
                          isSavingKnowledge={!!savingKnowledgeTurnIds[turn.id]}
                          canSaveKnowledge={embeddingAvailable}
                          onExecute={() => handleExecute(turn.id)}
                          onSaveView={() => handleSaveView(turn.id)}
                          onSaveKnowledge={() => handleSaveSqlToKnowledge(turn.id)}
                          t={t}
                        />
                        <RuleHitsPanel rules={turn.appliedRules} t={t} />
                        <ReferenceHitsPanel references={turn.usedReferences} t={t} />
                        <ValidationHitsPanel
                          score={turn.resultScore}
                          warnings={turn.validationWarnings}
                          suggestions={turn.validationSuggestions}
                          t={t}
                        />
                      </div>
                    )}

                    {/* Query Understanding panel (P3-Enterprise) */}
                    {turn.queryUnderstanding && !turn.isGenerating && !turn.clarificationQuestion && (
                      <div style={{ marginTop: 8 }}>
                        {/* F-12: Spell correction suggestions */}
                        {dataSourceTables(schemaData).length > 0 && (
                          <SpellCorrection
                            question={turn.question ?? ''}
                            schemaColumns={dataSourceTables(schemaData).flatMap((table) =>
                              (Array.isArray((table as { columns?: unknown[] }).columns) ? (table as { columns: Array<{ name?: string }> }).columns : []).map((column) => ({
                                table_name: String((table as { table_name?: string }).table_name ?? ''),
                                column_name: String(column.name ?? ''),
                              }))
                            )}
                          />
                        )}
                        {/* F-12: Semantic unreachable warning */}
                        <SemanticUnreachable qu={turn.queryUnderstanding} />
                        <QueryUnderstandingPanel
                          data={turn.queryUnderstanding}
                          originalQuestion={turn.question ?? ''}
                        />
                      </div>
                    )}

                    {/* Clarification input card should stay attached to the triggering assistant turn */}
                    {!turn.isGenerating && clarificationContext && effectiveClarifyingTurnId === turn.id && (
                      <div style={{ marginTop: 10 }}>
                        <ClarificationCard
                          context={clarificationContext}
                          onSelect={(opt) => handleClarify(opt)}
                          onFreeText={(text) => handleClarify(undefined, text)}
                          progressStage={turn.clarifyStage ?? null}
                          onCancel={() => {
                            const clarificationSessionId =
                              clarificationContext.conversation_id || conversationId || sessionId;
                            void nl2sqlApi.cancelClarify(clarificationSessionId).catch(() => {
                              message.warning(t('nl2sql.clarificationCancelSyncFailed'));
                            });
                            setClarificationContext(null);
                            setClarifyingTurnId(null);
                            setClarificationPausedByUser(true);
                            message.info(t('nl2sql.clarificationPausedQueue'));
                          }}
                          loading={clarifyLoading}
                          t={t}
                        />
                      </div>
                    )}

                    {/* Result: table + chart tabs */}
                    {!turn.isGenerating && turn.result && !turn.error && (
                      <div style={{ marginTop: 8 }}>
                        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
                          <Tag color="green" style={{ fontSize: 11 }}>
                            <CheckCircleOutlined style={{ marginRight: 4 }} />
                            {t('nl2sql.querySucceeded')}
                          </Tag>
                          {turn.result.row_count != null && (
                            <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
                              {t('nl2sql.resultCount', { count: turn.result.row_count })}
                            </Text>
                          )}
                          {/* Table / Chart tab switcher */}
                          <div style={{ marginLeft: 'auto' }}>
                            <Segmented
                              size="small"
                              value={getTurnViewTab(turn.id)}
                              onChange={(v) => setTurnViewTab(turn.id, v as ViewTab)}
                              options={[
                                {
                                  value: 'table',
                                  label: (
                                    <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}>
                                      <TableOutlined /> {t('nl2sql.table')}
                                    </span>
                                  ),
                                },
                                {
                                  value: 'chart',
                                  label: (
                                    <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}>
                                      <LineChartOutlined /> {t('nl2sql.chart')}
                                    </span>
                                  ),
                                },
                                {
                                  value: 'explain',
                                  label: (
                                    <span style={{ fontSize: 11, display: 'flex', alignItems: 'center', gap: 4 }}>
                                      <CommentOutlined /> {t('nl2sql.explain')}
                                    </span>
                                  ),
                                },
                              ]}
                            />
                          </div>
                        </div>

                        {getTurnViewTab(turn.id) === 'table' && (
                          (() => {
                            const pager = getTurnPager(turn.id);
                            const isMultiSourceTurn = turn.resultSource === 'agent' || (turn.multiSourceSteps?.length ?? 0) > 0;
                            const totalRows = turn.result?.total_rows ?? turn.result?.rows.length ?? 0;
                            const tableResult = turn.result;
                            return (
                              <>
                                <ResultTable
                                  result={tableResult!}
                                  onDownloadCSV={() => downloadCSV(turn.result!.columns, turn.result!.rows)}
                                  onDownloadExcel={() => downloadExcel(turn.result!.columns, turn.result!.rows)}
                                  onDownloadJSON={() => downloadJSON(turn.result!.columns, turn.result!.rows)}
                                  shareUrl={turn.queryId ? `${window.location.origin}${window.location.pathname}#/nl2sql?conversation_id=${encodeURIComponent(conversationId ?? '')}&query_id=${encodeURIComponent(turn.queryId)}` : undefined}
                                  t={t}
                                />
                                {turn.result && isMultiSourceTurn && (
                                  <div style={{ padding: '8px 16px 16px', display: 'flex', justifyContent: 'flex-end' }}>
                                    <Pagination
                                      current={pager.page}
                                      pageSize={pager.pageSize}
                                      total={totalRows}
                                      onChange={(p, ps) => {
                                        setTurnPager(turn.id, p, ps);
                                        if (isMultiSourceTurn) {
                                          void fetchMultiSourceResultPage(turn.id, p, ps);
                                        } else {
                                          handleExecute(turn.id, { page: p, pageSize: ps });
                                        }
                                      }}
                                      showSizeChanger
                                      hideOnSinglePage
                                      pageSizeOptions={[10, 20, 50, 100]}
                                      showTotal={(total, range) => `${range[0]}-${range[1]} of ${total} rows`}
                                    />
                                  </div>
                                )}
                              </>
                            );
                          })()
                        )}
                        {getTurnViewTab(turn.id) === 'chart' && (
                          <Suspense fallback={<div style={{ padding: 16, textAlign: 'center' }}><Spin size="small" /></div>}>
                            <LazyChartPanel
                              columns={turn.result!.columns}
                              rows={turn.result!.rows}
                              chartType={chartType}
                              onChartTypeChange={setChartType}
                            />
                          </Suspense>
                        )}
                        {getTurnViewTab(turn.id) === 'explain' && (
                          <ExplainTab
                            sql={turn.editedSql ?? turn.sql ?? null}
                            datasourceId={(turn.dataSourceId ?? selectedDataSourceId) ?? null}
                            queryId={resolveTurnQueryId(turn)}
                            t={t}
                          />
                        )}

                        {/* Feedback buttons */}
                        {resolveTurnQueryId(turn) && (
                          <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginTop: 8, paddingTop: 8, borderTop: '1px solid var(--border-subtle)' }}>
                            <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>{t('nl2sql.feedback.label')}</Text>
                            <Tooltip title={t('nl2sql.feedback.thumbsUp')}>
                              <Button
                                size="small"
                                type={turn.feedback === 'up' ? 'primary' : 'text'}
                                icon={<LikeOutlined />}
                                style={{ fontSize: 12 }}
                                onClick={() => {
                                  if (turn.feedback === 'up') return;
                                  nl2sqlApi.submitFeedback({
                                    conversationId: conversationId ?? '',
                                    datasourceId: turn.dataSourceId ?? selectedDataSourceId ?? '',
                                    generatedSql: turn.sql ?? '',
                                    feedbackType: 'thumbs_up',
                                  }).catch(() => {});
                                  setTurns(prev => prev.map(t => t.id === turn.id ? { ...t, feedback: 'up' } : t));
                                }}
                              />
                            </Tooltip>
                            <Tooltip title={t('nl2sql.feedback.thumbsDown')}>
                              <Button
                                size="small"
                                type={turn.feedback === 'down' ? 'primary' : 'text'}
                                danger={turn.feedback === 'down'}
                                icon={<DislikeOutlined />}
                                style={{ fontSize: 12 }}
                                onClick={() => {
                                  if (turn.feedback === 'down') return;
                                  nl2sqlApi.submitFeedback({
                                    conversationId: conversationId ?? '',
                                    datasourceId: turn.dataSourceId ?? selectedDataSourceId ?? '',
                                    generatedSql: turn.sql ?? '',
                                    feedbackType: 'thumbs_down',
                                  }).catch(() => {});
                                  setTurns(prev => prev.map(t => t.id === turn.id ? { ...t, feedback: 'down' } : t));
                                  setFeedbackReasonByTurn((prev) => ({ ...prev, [turn.id]: prev[turn.id] ?? null }));
                                }}
                              />
                            </Tooltip>
                          </div>
                        )}
                        {turn.feedback === 'down' && !feedbackReasonByTurn[turn.id] && (
                          <div style={{
                            marginTop: 8,
                            padding: '8px 10px',
                            borderRadius: 8,
                            border: '1px solid rgba(251, 146, 60, 0.35)',
                            background: 'rgba(251, 146, 60, 0.08)',
                            display: 'flex',
                            flexDirection: 'column',
                            gap: 6,
                          }}>
                            <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
                              {t('nl2sql.feedback.reasonPrompt')}
                            </Text>
                            <Space size={[6, 6]} wrap>
                              {feedbackReasonOptions.map((opt) => {
                                const selected = feedbackReasonByTurn[turn.id] === opt.key;
                                return (
                                  <Button
                                    key={`fb-reason-${turn.id}-${opt.key}`}
                                    size="small"
                                    type={selected ? 'primary' : 'default'}
                                    disabled={selected}
                                    icon={selected ? <CheckOutlined /> : undefined}
                                    onClick={() => {
                                      if (selected) return;
                                      const dsId = turn.dataSourceId ?? selectedDataSourceId ?? '';
                                      if (!dsId) return;
                                      nl2sqlApi.submitFeedback({
                                        conversationId: conversationId ?? '',
                                        datasourceId: dsId,
                                        generatedSql: turn.sql ?? '',
                                        feedbackType: feedbackTypeFromReason(opt.key),
                                        correctionNote: opt.label,
                                      }).catch(() => {});
                                      setFeedbackReasonByTurn((prev) => ({ ...prev, [turn.id]: opt.key }));
                                      message.success(t('nl2sql.feedback.reasonSubmitted'));
                                    }}
                                  >
                                    {selected ? t('nl2sql.feedback.reasonDone') : opt.label}
                                  </Button>
                                );
                              })}
                            </Space>
                          </div>
                        )}
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          ))}

          <div ref={messagesEndRef} />
        </div>

        {/* Input */}
        <div style={{
          padding: '12px 14px 16px', borderTop: '1px solid var(--border-subtle)', flexShrink: 0,
        }}>
          <div style={{
            marginBottom: 8,
            padding: '6px 10px',
            borderRadius: 8,
            border: '1px solid rgba(59, 130, 246, 0.25)',
            background: 'rgba(59, 130, 246, 0.08)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            gap: 8,
          }}>
            <Text style={{ fontSize: 12, color: 'var(--text-primary)' }}>
              {modeStatusText}
            </Text>
            <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
              {defaultModeText}
            </Text>
          </div>
          {/* Data source quick-switcher + AI suggestion banner */}
          {dataSources.length > 1 && (
            <div style={{ marginBottom: 8 }}>
              {/* Auto-routing toggle */}
              <div style={{
                display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                marginBottom: suggestedSource || isSuggestingSource ? 6 : 0,
              }}>
                <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
                  <RobotOutlined style={{ color: '#7c3aed', marginRight: 4 }} />
                  {t('nl2sql.autoRouting')}
                </Text>
                <Switch
                  checked={autoRouting}
                  disabled={embeddingRequiredBlocked}
                  onChange={(checked) => {
                    setAutoRouting(checked);
                    if (!checked) {
                      setSuggestedSource(null);
                    }
                  }}
                  size="small"
                />
              </div>

              <Select
                placeholder={t('nl2sql.selectDataSourcePlaceholder')}
                value={selectedDataSourceId}
                onChange={(val) => { setSelectedDataSourceId(val); setSuggestedSource(null); }}
                style={{ width: '100%' }}
                size="small"
                allowClear
                suffixIcon={<DatabaseOutlined />}
                options={dataSources.map(ds => ({
                  value: ds.id,
                  label: (
                    <span>
                      <DatabaseOutlined style={{ marginRight: 6, color: 'var(--text-muted)' }} />
                      {ds.name}
                      <Tag style={{ marginLeft: 6, fontSize: 9 }}>{ds.db_type}</Tag>
                    </span>
                  ),
                }))}
              />
            </div>
          )}

          <div style={{ marginBottom: advancedSettingsOpen ? 8 : 6 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 6, minHeight: 24, flexWrap: 'wrap' }}>
              <Button
                size="small"
                type="text"
                icon={advancedSettingsOpen ? <UpOutlined /> : <DownOutlined />}
                onClick={() => {
                  setAdvancedSettingsOpen((open) => {
                    if (open) setReferencePopoverOpen(false);
                    return !open;
                  });
                }}
                style={{ height: 24, paddingInline: 6, fontSize: 11, color: 'var(--text-secondary)' }}
              >
                {t('nl2sql.advancedSettings')}
              </Button>
              {!advancedSettingsOpen && referenceOverrideActive && (
                <Tag color="blue" style={{ marginInlineEnd: 0, fontSize: 10 }}>
                  {selectedReferenceSummary}
                </Tag>
              )}
            </div>

            {advancedSettingsOpen && (
              <div style={{
                marginTop: 6,
                padding: '8px 10px',
                borderRadius: 8,
                border: '1px solid var(--border-subtle)',
                background: 'var(--bg-secondary)',
                display: 'grid',
                gap: 8,
              }}>
                <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8, flexWrap: 'wrap' }}>
                  <div style={{ minWidth: 0 }}>
                    <Text style={{ display: 'block', fontSize: 12, color: 'var(--text-primary)' }}>
                      {t('nl2sql.references.overrideTitle')}
                    </Text>
                    <Text style={{ display: 'block', fontSize: 11, color: 'var(--text-muted)' }}>
                      {t('nl2sql.references.overrideHint')}
                    </Text>
                  </div>
                  <Popover
                    trigger="click"
                    placement="topLeft"
                    open={referencePopoverOpen}
                    onOpenChange={(open) => {
                      if (!selectedDataSourceId && open) {
                        message.info(t('nl2sql.references.selectDataSourceFirst'));
                        return;
                      }
                      setReferencePopoverOpen(open);
                    }}
                    content={
                      selectedDataSourceId ? (
                        <ReferenceBindingPopover
                          packs={referencePacks}
                          selectedPackIds={selectedReferencePackIds}
                          selectedFileIds={selectedReferenceFileIds}
                          includeAll={includeAllReferences}
                          loading={referencePacksLoading}
                          uploadingPackId={uploadingReferencePackId}
                          onToggleAll={(checked) => {
                            setIncludeAllReferences(checked);
                            if (checked) {
                              setSelectedReferencePackIds([]);
                              setSelectedReferenceFileIds([]);
                            }
                          }}
                          onChangePacks={setSelectedReferencePackIds}
                          onChangeFiles={setSelectedReferenceFileIds}
                          onCreatePack={(name) => createReferencePackMutation.mutate(name)}
                          onUploadFile={(packId, file) => uploadReferenceMutation.mutate({ packId, file })}
                          onTogglePackEnabled={(pack) =>
                            updateReferencePackMutation.mutate({ pack, enabled: !pack.enabled })
                          }
                          onDeletePack={(pack) => deleteReferencePackMutation.mutate(pack)}
                          onDeleteFile={(fileId) => deleteReferenceFileMutation.mutate(fileId)}
                          t={t}
                        />
                      ) : null
                    }
                  >
                    <Button
                      size="small"
                      icon={<PaperClipOutlined />}
                      disabled={!selectedDataSourceId}
                      type={referenceOverrideActive ? 'primary' : 'default'}
                      style={{ height: 26, fontSize: 11, borderRadius: 6 }}
                    >
                      {t('nl2sql.references.bindButton')}
                    </Button>
                  </Popover>
                </div>
                <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                  <Tag style={{ marginInlineEnd: 0, fontSize: 10 }}>
                    {selectedReferenceSummary}
                  </Tag>
                  {!includeAllReferences && selectedReferencePackIds.slice(0, 2).map((id) => (
                    <Tag key={`ref-pack-chip-${id}`} color="blue" style={{ marginInlineEnd: 0, fontSize: 10 }}>
                      {referencePackNameById.get(id) ?? id}
                    </Tag>
                  ))}
                  {!includeAllReferences && selectedReferenceFileIds.slice(0, 2).map((id) => (
                    <Tag key={`ref-file-chip-${id}`} color="cyan" style={{ marginInlineEnd: 0, fontSize: 10 }}>
                      {referenceFileNameById.get(id) ?? id}
                    </Tag>
                  ))}
                  {!includeAllReferences && selectedReferencePackIds.length + selectedReferenceFileIds.length > 4 && (
                    <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                      +{selectedReferencePackIds.length + selectedReferenceFileIds.length - 4}
                    </Text>
                  )}
                </div>
              </div>
            )}
          </div>

          <div style={{ display: 'flex', gap: 8, alignItems: 'flex-end' }}>
            <TextArea
              ref={inputRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={
                embeddingRequiredBlocked
                  ? t('nl2sql.embeddingRequiredInputPlaceholder')
                  : dataSources.length === 0
                  ? t('nl2sql.noDataSourcesConfigured')
                  : selectedDataSourceId || suggestedSource
                  ? t('nl2sql.inputPlaceholder')
                  : t('nl2sql.inputPlaceholderAuto')
              }
              autoSize={{ minRows: 1, maxRows: 4 }}
              disabled={embeddingRequiredBlocked}
              style={{
                flex: 1, borderRadius: 10, fontSize: 14,
                border: '1px solid var(--border-default)',
                background: 'var(--bg-elevated)',
                color: 'var(--text-primary)',
                resize: 'none', fontFamily: 'var(--font-ui)', padding: '10px 14px',
              }}
            />
            <Button
              type="primary"
              icon={<SendOutlined />}
              onClick={() => handleSend()}
              disabled={embeddingRequiredBlocked || !input.trim() || dataSources.length === 0}
              style={{ height: 40, width: 40, borderRadius: 10, display: 'flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0 }}
            />
          </div>

          <PromptQueuePanel
            items={queuedPrompts.filter((item) => item.id !== activeQueueItemId)}
            processing={isQueueProcessing}
            activeItemId={activeQueueItemId}
            pendingText={t('nl2sql.queuePending', { count: Math.max(queuedPrompts.length - (isQueueProcessing ? 1 : 0), 0) })}
            labels={{
              title: t('nl2sql.queueTitle'),
              processing: t('nl2sql.queueProcessing'),
              delete: t('nl2sql.queueDelete'),
              inputPlaceholder: t('nl2sql.queueInputPlaceholder'),
              lockedHint: t('nl2sql.queueLockedHint'),
              collapse: t('nl2sql.queueCollapse'),
              expand: t('nl2sql.queueExpand'),
              retryTag: 'retry',
            }}
            onUpdateItem={updateQueuedPrompt}
            onDeleteItem={removeQueuedPrompt}
          />
          {queuedPrompts.length > 0 && (!!clarificationContext || clarificationPausedByUser) && (
            <div style={{
              marginTop: 8,
              padding: '8px 10px',
              borderRadius: 8,
              border: '1px solid rgba(245, 158, 11, 0.35)',
              background: 'rgba(245, 158, 11, 0.08)',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 8,
            }}>
              <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
                {t('nl2sql.queuePausedByClarification')}
              </Text>
              <Space size={6}>
                <Button
                  size="small"
                  onClick={() => {
                    clearPromptQueue();
                    setClarificationPausedByUser(false);
                  }}
                >
                  {t('nl2sql.queueClear')}
                </Button>
                <Button
                  size="small"
                  type="primary"
                  disabled={!!clarificationContext}
                  onClick={() => {
                    setClarificationPausedByUser(false);
                    resumePromptQueue();
                  }}
                >
                  {t('nl2sql.queueResume')}
                </Button>
              </Space>
            </div>
          )}

          {showSlashMenu && (
            <div style={{
              marginTop: 8,
              padding: 10,
              border: '1px solid var(--border-subtle)',
              borderRadius: 10,
              background: 'var(--bg-elevated)',
              display: 'flex',
              flexDirection: 'column',
              gap: 8,
            }}>
              <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
                {t('nl2sql.slashCommandsTitle')}
              </Text>
              {filteredSlashCommands.length > 0 ? (
                <>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                    {filteredSlashCommands.map((cmd, idx) => {
                      const active = idx === slashActiveIndex;
                      return (
                        <button
                          key={cmd.key}
                          type="button"
                          onClick={() => applySlashCommand(cmd.key)}
                          style={{
                            textAlign: 'left',
                            border: active ? '1px solid var(--accent-ai)' : '1px solid var(--border-subtle)',
                            background: active ? 'rgba(124,58,237,0.14)' : 'transparent',
                            color: 'var(--text-primary)',
                            borderRadius: 8,
                            padding: '8px 10px',
                            cursor: 'pointer',
                            display: 'flex',
                            justifyContent: 'space-between',
                            alignItems: 'center',
                            fontFamily: 'var(--font-ui)',
                          }}
                        >
                          <span style={{ fontSize: 12, fontWeight: 600 }}>{cmd.label}</span>
                          <span style={{ fontSize: 11, color: 'var(--text-muted)' }}>{cmd.desc}</span>
                        </button>
                      );
                    })}
                  </div>
                  <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                    {t('nl2sql.slashKeyboardHint')}
                  </Text>
                </>
              ) : (
                <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
                  {t('nl2sql.slashNoMatch')}
                </Text>
              )}
            </div>
          )}

          <div style={{ marginTop: 6, fontSize: 11, color: 'var(--text-muted)', display: 'flex', gap: 12, flexWrap: 'wrap' }}>
            <span>⏎ {t('nl2sql.sendHint')}</span>
            <span>⇧⏎ {t('nl2sql.newLineHint')}</span>
            <span>{t('nl2sql.slashCommandsHint')}</span>
            <span>/multi + 问题：强制多数据源</span>
          </div>
        </div>
      </div>

      {/* ── Right rail: Schema drawer trigger ── */}
      <div style={{
        width: rightRailWidth,
        borderLeft: '1px solid var(--border-subtle)',
        background: 'var(--bg-surface)',
        display: 'flex', flexDirection: 'column',
        overflow: 'hidden', flexShrink: 0,
      }}>
        <div style={{
          padding: '10px 0',
          borderBottom: '1px solid var(--border-subtle)',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
        }}>
          <Tooltip title={t('nl2sql.schema')}>
            <Button
              type="text" size="small"
              icon={<InfoCircleOutlined />}
              onClick={() => setSchemaDrawerOpen(true)}
              style={{ color: 'var(--text-muted)', padding: '0 4px', height: 24, width: 24, minWidth: 24 }}
            />
          </Tooltip>
        </div>
      </div>

      {/* ── Drawers ── */}
      <ConversationDrawer
        open={conversationDrawerOpen}
        onClose={() => setConversationDrawerOpen(false)}
        onSelectConversation={handleLoadConversation}
        t={t}
      />
      <SavedViewsDrawer
        open={savedViewsDrawerOpen}
        onClose={() => setSavedViewsDrawerOpen(false)}
        views={savedViews}
        onLoad={handleLoadView}
        onDelete={async (id) => {
          await deleteViewMutation.mutateAsync(id);
          await refetchSavedViews();
        }}
        onRename={(id, data) => renameViewMutation.mutate({ id, data })}
        t={t}
      />
      <Drawer
        open={schemaDrawerOpen}
        onClose={() => setSchemaDrawerOpen(false)}
        title={
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <InfoCircleOutlined style={{ color: 'var(--accent-ai)' }} />
            <span>{t('nl2sql.schema')}</span>
            {selectedDataSource && (
              <Tag style={{ fontSize: 10, marginLeft: 4 }}>{selectedDataSource.db_type}</Tag>
            )}
          </div>
        }
        placement="right"
        width={380}
        styles={{ body: { padding: '8px 10px', background: 'var(--bg-surface)' } }}
      >
        {selectedDataSource ? (
          dataSourceTables(selectedDataSource).length > 0 ? (
            <Collapse ghost size="small">
              {dataSourceTables(selectedDataSource).map((table) => {
                const columns = Array.isArray((table as { columns?: unknown[] }).columns)
                  ? (table as { columns: Array<{ name: string; type: string; primary_key?: boolean; nullable?: boolean }> }).columns
                  : [];
                return (
                <Panel
                  key={String((table as { table_name?: string }).table_name ?? '')}
                  header={
                    <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                      <TableOutlined style={{ fontSize: 11, color: 'var(--accent-ai)' }} />
                      <span style={{ fontSize: 12, fontWeight: 600 }}>{String((table as { table_name?: string }).table_name ?? '')}</span>
                      <Tag style={{ fontSize: 9, marginLeft: 'auto' }}>
                        {columns.length} cols
                      </Tag>
                    </div>}
                >
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                    {columns.map((col) => (
                      <div key={col.name} style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                        <Text style={{ fontSize: 11, color: 'var(--text-secondary)', flex: 1, fontFamily: 'var(--font-code)' }}>
                          {col.name}
                        </Text>
                        <Tag style={{ fontSize: 9, padding: '0 4px' }}>{col.type}</Tag>
                        {col.primary_key && <Tag color="gold" style={{ fontSize: 9, padding: '0 4px' }}>PK</Tag>}
                        {col.nullable === false && <Tag color="blue" style={{ fontSize: 9, padding: '0 4px' }}>{t('nl2sql.notNull')}</Tag>}
                      </div>
                    ))}
                  </div>
                </Panel>
                );
              })}
            </Collapse>
          ) : (
            <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-muted)', fontSize: 12 }}>
              <DatabaseOutlined style={{ fontSize: 24, display: 'block', marginBottom: 8 }} />
              {t('nl2sql.noSchema')}
              <div style={{ marginTop: 8 }}>
                <Button
                  size="small"
                  icon={<ThunderboltOutlined />}
                  onClick={async () => {
                    if (!selectedDataSourceId) return;
                    try {
                      await dataSourcesApi.discoverSchema(selectedDataSourceId);
                      message.success(t('nl2sql.schemaDiscovered'));
                      await qc.invalidateQueries({ queryKey: queryKeys.dataSources.list() });
                    } catch {
                      message.error(t('nl2sql.schemaDiscoverFailed'));
                    }
                  }}
                  style={{ fontSize: 11 }}
                >
                  {t('nl2sql.discoverSchema')}
                </Button>
              </div>
            </div>
          )
        ) : (
          <div style={{ padding: 24, textAlign: 'center', color: 'var(--text-muted)', fontSize: 12 }}>
            <DatabaseOutlined style={{ fontSize: 24, display: 'block', marginBottom: 8 }} />
            {t('nl2sql.selectDataSourceForSchema')}
          </div>
        )}
      </Drawer>
    </Layout>
      </div>
    </div>
    </ErrorBoundary>
  );
}
