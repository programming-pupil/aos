import { Space, Tag, Typography } from 'antd';
import { useTranslation } from 'react-i18next';
import type { RdTaskEvent } from '@/types';
import { runtimeStringArray } from './utils';

const { Text } = Typography;

export function contextCacheUsageDetail(event: RdTaskEvent): Record<string, unknown> | null {
  if (event.stage !== 'context_cache_usage' && event.stage !== 'context_retrieval_evidence') {
    return null;
  }
  return event.detailJson && typeof event.detailJson === 'object' && !Array.isArray(event.detailJson)
    ? event.detailJson
    : null;
}

export function contextCacheNumber(detail: Record<string, unknown>, keys: string[]): number {
  for (const key of keys) {
    const value = detail[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return 0;
}

export function ContextCacheUsage({ event }: { event: RdTaskEvent }) {
  const { t } = useTranslation();
  const detail = contextCacheUsageDetail(event);
  if (!detail) return null;
  const selectedFiles = contextCacheNumber(detail, ['selectedFiles', 'fileCount']);
  const embeddingHits = contextCacheNumber(detail, ['embeddingHits', 'embeddingHitCount']);
  const lexicalHits = contextCacheNumber(detail, ['lexicalHits', 'lexicalHitCount']);
  const summaryHits = contextCacheNumber(detail, ['summaryHits', 'summaryHitCount']);
  const symbolHits = contextCacheNumber(detail, ['symbolHits', 'symbolHitCount']);
  const importHits = contextCacheNumber(detail, ['importHits', 'importHitCount']);
  const dependencyGraphHits = contextCacheNumber(detail, ['dependencyGraphHits', 'dependencyGraphHitCount']);
  const taskMemoryHits = contextCacheNumber(detail, ['taskMemoryHits', 'taskMemoryHitCount']);
  const mergedCandidates = contextCacheNumber(detail, ['mergedCandidates']);
  const staleFiles = contextCacheNumber(detail, ['staleFiles']);
  const cacheReusedChunks = contextCacheNumber(detail, ['cacheReusedChunks']);
  const cacheRegeneratedChunks = contextCacheNumber(detail, ['cacheRegeneratedChunks']);
  const estimatedTokensSaved = contextCacheNumber(detail, ['estimatedTokensSaved']);
  const sources = runtimeStringArray(detail.cacheSources ?? detail.sources).slice(0, 6);
  const cacheMissReasons = runtimeStringArray(detail.cacheMissReasons).slice(0, 4);
  return (
    <Space direction="vertical" size={4} style={{ marginTop: 6 }}>
      <Space size={[6, 6]} wrap>
        <Tag color="geekblue">{t('rd.selectedCandidateFiles', '候选文件')}: {selectedFiles}</Tag>
        <Tag color="blue">{t('rd.mergedCandidates', '合并候选')}: {mergedCandidates}</Tag>
        <Tag color={embeddingHits > 0 ? 'green' : 'default'}>{t('rd.embeddingHits', 'Embedding 命中')}: {embeddingHits}</Tag>
        <Tag color={lexicalHits > 0 ? 'cyan' : 'default'}>{t('rd.lexicalHits', '词法/索引命中')}: {lexicalHits}</Tag>
        <Tag color={summaryHits > 0 ? 'lime' : 'default'}>{t('rd.summaryHits', '摘要命中')}: {summaryHits}</Tag>
        <Tag color={symbolHits > 0 ? 'purple' : 'default'}>{t('rd.symbolHits', 'Symbol 命中')}: {symbolHits}</Tag>
        <Tag color={importHits > 0 ? 'magenta' : 'default'}>{t('rd.importHits', 'Import 命中')}: {importHits}</Tag>
        <Tag color={dependencyGraphHits > 0 ? 'orange' : 'default'}>{t('rd.dependencyGraphHits', '依赖图命中')}: {dependencyGraphHits}</Tag>
        <Tag color={taskMemoryHits > 0 ? 'gold' : 'default'}>{t('rd.taskMemoryHits', '历史任务命中')}: {taskMemoryHits}</Tag>
        <Tag color={staleFiles > 0 ? 'red' : 'default'}>{t('rd.staleFiles', '疑似过期文件')}: {staleFiles}</Tag>
        <Tag color="green">{t('rd.cacheReusedChunks', '复用 Chunk')}: {cacheReusedChunks}</Tag>
        <Tag color={cacheRegeneratedChunks > 0 ? 'volcano' : 'default'}>{t('rd.cacheRegeneratedChunks', '重建 Chunk')}: {cacheRegeneratedChunks}</Tag>
        <Tag color="lime">{t('rd.estimatedTokensSaved', '估算节省 Token')}: {estimatedTokensSaved.toLocaleString()}</Tag>
      </Space>
      {sources.length > 0 ? (
        <Text style={{ color: '#64748b', fontSize: 11 }}>
          {t('rd.cacheSources', '来源')}: {sources.join(', ')}
        </Text>
      ) : null}
      {cacheMissReasons.length > 0 ? (
        <Space direction="vertical" size={2}>
          <Text style={{ color: '#94a3b8', fontSize: 11 }}>{t('rd.cacheMissReasons', '缓存未命中原因')}</Text>
          {cacheMissReasons.map((reason) => (
            <Text key={reason} style={{ color: '#64748b', fontSize: 11 }}>- {reason}</Text>
          ))}
        </Space>
      ) : null}
      <Text style={{ color: '#64748b', fontSize: 11 }}>
        {t('rd.cacheUsageEffectFirstNote', '缓存只用于定位候选上下文；关键结论和代码修改仍需读取真实文件核对。')}
      </Text>
    </Space>
  );
}
