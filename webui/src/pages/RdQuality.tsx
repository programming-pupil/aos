import { useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { Alert, Card, Empty, Select, Space, Spin, Tag, Typography } from 'antd';
import {
  ApiOutlined,
  BarChartOutlined,
  BranchesOutlined,
  CheckCircleOutlined,
  ClockCircleOutlined,
  DatabaseOutlined,
  ExperimentOutlined,
  FileTextOutlined,
  FolderOpenOutlined,
  SearchOutlined,
  ThunderboltOutlined,
} from '@ant-design/icons';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';

const { Text, Title } = Typography;

function formatPercent(value?: number | null) {
  return `${Math.round((value ?? 0) * 100)}%`;
}

function formatDurationMs(value?: number | null) {
  if (!value || value <= 0) return '-';
  if (value >= 60_000) return `${(value / 60_000).toFixed(1)}m`;
  if (value >= 1000) return `${(value / 1000).toFixed(1)}s`;
  return `${Math.round(value)}ms`;
}

function formatInteger(value?: number | null) {
  return Math.max(0, Math.round(value ?? 0)).toLocaleString();
}

function MetricCard({ label, value, color, icon }: { label: string; value: string | number; color: string; icon: ReactNode }) {
  return (
    <Card
      style={{ background: 'rgba(15, 23, 42, 0.76)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
      styles={{ body: { padding: 16 } }}
    >
      <Space direction="vertical" size={8} style={{ width: '100%' }}>
        <Space style={{ justifyContent: 'space-between', width: '100%' }}>
          <Text style={{ color: '#94a3b8' }}>{label}</Text>
          <span style={{ color }}>{icon}</span>
        </Space>
        <div style={{ color, fontSize: 28, fontWeight: 800, letterSpacing: -0.5 }}>{value}</div>
      </Space>
    </Card>
  );
}

export default function RdQuality() {
  const { t } = useTranslation();
  const [repositoryId, setRepositoryId] = useState<string | undefined>();
  const [days, setDays] = useState(30);

  const repositoriesQuery = useQuery({
    queryKey: queryKeys.rd.repositories(),
    queryFn: rdApi.listRepositories,
  });
  const repositories = repositoriesQuery.data?.repositories ?? [];
  const selectedRepository = repositories.find((repo) => repo.id === repositoryId) ?? null;
  const qualityParams = useMemo(() => ({ days, repositoryId }), [days, repositoryId]);
  const qualityQuery = useQuery({
    queryKey: queryKeys.rd.quality(qualityParams),
    queryFn: () => rdApi.quality(qualityParams),
    refetchInterval: 15000,
  });
  const quality = qualityQuery.data;

  const metrics = quality ? [
    { label: t('rd.taskSuccessRate', '任务成功率'), value: formatPercent(quality.successRate), color: '#86efac', icon: <CheckCircleOutlined /> },
    { label: t('rd.testPassRate', '测试通过率'), value: formatPercent(quality.testPassRate), color: '#93c5fd', icon: <ExperimentOutlined /> },
    { label: t('rd.pendingDiffs', '待审批 Diff'), value: quality.pendingDiffCount, color: quality.pendingDiffCount > 0 ? '#facc15' : '#cbd5e1', icon: <BranchesOutlined /> },
    { label: t('rd.failedTasks', '失败任务'), value: quality.failedCount, color: quality.failedCount > 0 ? '#fca5a5' : '#cbd5e1', icon: <ThunderboltOutlined /> },
    { label: t('rd.avgDuration', '平均耗时'), value: formatDurationMs(quality.avgTaskDurationMs), color: '#c4b5fd', icon: <ClockCircleOutlined /> },
    { label: t('rd.runningTasks', '运行中'), value: quality.runningCount, color: quality.runningCount > 0 ? '#5eead4' : '#cbd5e1', icon: <ThunderboltOutlined /> },
    { label: t('rd.candidateDiffs', '候选 Diff'), value: quality.candidateWorktreeDiffCount ?? 0, color: '#67e8f9', icon: <BranchesOutlined /> },
    { label: t('rd.diffCheckPassRate', 'Diff 校验'), value: formatPercent(quality.diffCheckPassRate ?? 0), color: '#bbf7d0', icon: <CheckCircleOutlined /> },
    { label: t('rd.reviewAgentRuns', 'Review 次数'), value: quality.reviewAgentRunCount ?? 0, color: '#f9a8d4', icon: <FileTextOutlined /> },
    { label: t('rd.reviewLineRefs', '行级定位'), value: quality.reviewLineRefCount ?? 0, color: '#fdba74', icon: <FileTextOutlined /> },
    { label: t('rd.cacheHitFiles', '缓存命中文件'), value: formatInteger(quality.retrievalSelectedFileCount), color: '#7dd3fc', icon: <SearchOutlined /> },
    { label: t('rd.cacheHitRate', '缓存命中率'), value: formatPercent(quality.cacheHitRate ?? 0), color: '#86efac', icon: <SearchOutlined /> },
    { label: t('rd.totalTokenUsage', 'Token 消耗'), value: formatInteger(quality.totalTokens), color: '#fde68a', icon: <ApiOutlined /> },
  ] : [];
  const cacheMetrics = quality ? [
    { label: t('rd.cacheUsageEvents', '缓存使用事件'), value: quality.cacheUsageEventCount ?? 0, color: 'cyan' },
    { label: t('rd.retrievalEvidenceEvents', '召回证据事件'), value: quality.retrievalEvidenceEventCount ?? 0, color: 'blue' },
    { label: t('rd.selectedCandidateFiles', '候选文件'), value: quality.retrievalSelectedFileCount ?? 0, color: 'geekblue' },
    { label: t('rd.embeddingHits', 'Embedding 命中'), value: quality.embeddingHitCount ?? 0, color: 'green' },
    { label: t('rd.summaryHits', '摘要命中'), value: quality.summaryHitCount ?? 0, color: 'lime' },
    { label: t('rd.symbolHits', 'Symbol 命中'), value: quality.symbolHitCount ?? 0, color: 'purple' },
    { label: t('rd.importHits', 'Import 命中'), value: quality.importHitCount ?? 0, color: 'magenta' },
    { label: t('rd.dependencyGraphHits', '依赖图命中'), value: quality.dependencyGraphHitCount ?? 0, color: 'orange' },
    { label: t('rd.taskMemoryHits', '历史任务命中'), value: quality.taskMemoryHitCount ?? 0, color: 'gold' },
    { label: t('rd.cacheReusedChunks', '复用 Chunk'), value: quality.cacheReusedChunkCount ?? 0, color: 'green' },
    { label: t('rd.cacheRegeneratedChunks', '重建 Chunk'), value: quality.cacheRegeneratedChunkCount ?? 0, color: 'volcano' },
    { label: t('rd.estimatedTokensSaved', '估算节省 Token'), value: quality.estimatedTokensSaved ?? 0, color: 'lime' },
  ] : [];
  const embeddingMetrics = quality ? [
    { label: t('rd.embeddingContextSummaryChunks', '仓库/目录摘要向量'), value: quality.embeddingContextSummaryChunkCount ?? 0 },
    { label: t('rd.embeddingFileSummaryChunks', '文件摘要向量'), value: quality.embeddingFileSummaryChunkCount ?? 0 },
    { label: t('rd.embeddingSymbolChunks', 'Symbol 向量'), value: quality.embeddingSymbolChunkCount ?? 0 },
    { label: t('rd.embeddingImportChunks', 'Import 向量'), value: quality.embeddingImportChunkCount ?? 0 },
    { label: t('rd.embeddingTaskChunks', '历史任务向量'), value: quality.embeddingTaskChunkCount ?? 0 },
    { label: t('rd.embeddingReusedChunks', 'Embedding 复用'), value: quality.embeddingReusedChunkCount ?? 0 },
    { label: t('rd.embeddingRegeneratedChunks', 'Embedding 重建'), value: quality.embeddingRegeneratedChunkCount ?? 0 },
    { label: t('rd.embeddingPrunedChunks', 'Embedding 清理'), value: quality.embeddingPrunedChunkCount ?? 0 },
  ] : [];
  const summaryMetrics = quality ? [
    { label: t('rd.repositorySummaryCount', '仓库/入口摘要'), value: quality.repositorySummaryCount ?? 0 },
    { label: t('rd.directorySummaryCount', '目录摘要'), value: quality.directorySummaryCount ?? 0 },
    { label: t('rd.fileSummaryCount', '文件摘要'), value: quality.fileSummaryCount ?? 0 },
    { label: t('rd.llmSummaryCount', 'LLM 精炼摘要'), value: quality.llmSummaryCount ?? 0 },
    { label: t('rd.pendingLlmSummaryCount', '待 LLM 精炼'), value: quality.staleSummaryCount ?? 0 },
    { label: t('rd.fileSummaryReusedCount', '文件摘要复用'), value: quality.fileSummaryReusedCount ?? 0 },
    { label: t('rd.fileSummaryRegeneratedCount', '文件摘要重建'), value: quality.fileSummaryRegeneratedCount ?? 0 },
  ] : [];
  const tokenMetrics = quality ? [
    { label: t('rd.runtimeTokens', 'Runtime'), value: quality.runtimeTokens ?? 0, color: 'cyan' },
    { label: t('rd.contextPlannerTokens', 'Context Planner'), value: quality.contextPlannerTokens ?? 0, color: 'purple' },
    { label: t('rd.embeddingTokens', 'Embedding'), value: quality.embeddingTokens ?? 0, color: 'green' },
    { label: t('rd.inputTokens', '输入'), value: quality.inputTokens ?? 0, color: 'blue' },
    { label: t('rd.outputTokens', '输出'), value: quality.outputTokens ?? 0, color: 'geekblue' },
    { label: t('rd.cacheWriteTokens', '缓存写入'), value: quality.cacheCreationTokens ?? 0, color: 'gold' },
    { label: t('rd.cacheReadTokens', '缓存读取'), value: quality.cacheReadTokens ?? 0, color: 'lime' },
  ] : [];
  const toolMetrics = quality ? [
    { label: 'read_file', value: quality.readFileCount ?? 0, color: 'cyan' },
    { label: 'grep_search', value: quality.grepSearchCount ?? 0, color: 'blue' },
    { label: 'glob_search', value: quality.globSearchCount ?? 0, color: 'geekblue' },
    { label: t('rd.repeatedToolTargets', '重复目标'), value: quality.repeatedToolTargetCount ?? 0, color: (quality.repeatedToolTargetCount ?? 0) > 0 ? 'orange' : 'default' },
  ] : [];

  return (
    <div
      style={{
        minHeight: '100%',
        padding: 22,
        background:
          'radial-gradient(circle at 12% 6%, rgba(56, 189, 248, 0.16), transparent 30%), radial-gradient(circle at 88% 8%, rgba(34, 197, 94, 0.12), transparent 28%), #050914',
      }}
    >
      <Space direction="vertical" size={16} style={{ width: '100%' }}>
        <Card
          style={{ background: 'rgba(15, 23, 42, 0.82)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
          styles={{ body: { padding: 22 } }}
        >
          <Space wrap style={{ justifyContent: 'space-between', width: '100%', alignItems: 'flex-start' }}>
            <Space direction="vertical" size={6}>
              <Tag color="cyan">R&D Quality</Tag>
              <Title level={2} style={{ color: '#f8fafc', margin: 0 }}>{t('rd.qualityPageTitle', '研发质量快照')}</Title>
              <Text style={{ color: '#94a3b8' }}>{t('rd.qualityPageDesc', '独立查看代码任务、Diff、测试和 Review Agent 的质量指标，不干扰 代码开发主流程。')}</Text>
            </Space>
            <Space wrap>
              <Select
                allowClear
                value={repositoryId}
                loading={repositoriesQuery.isLoading}
                onChange={setRepositoryId}
                style={{ minWidth: 240 }}
                placeholder={t('rd.allRepositories', '全部仓库')}
                options={repositories.map((repo) => ({ value: repo.id, label: `${repo.name} · ${repo.branch}` }))}
              />
              <Select
                value={days}
                onChange={setDays}
                style={{ width: 130 }}
                options={[
                  { value: 7, label: t('rd.lastDays', '近 {{days}} 天', { days: 7 }) },
                  { value: 30, label: t('rd.lastDays', '近 {{days}} 天', { days: 30 }) },
                  { value: 90, label: t('rd.lastDays', '近 {{days}} 天', { days: 90 }) },
                ]}
              />
            </Space>
          </Space>
        </Card>

        {selectedRepository ? (
          <Alert
            type="info"
            showIcon
            message={selectedRepository.name}
            description={<span><FolderOpenOutlined /> {selectedRepository.url} · {selectedRepository.branch}</span>}
          />
        ) : null}

        {qualityQuery.isLoading ? (
          <Card style={{ background: 'rgba(15, 23, 42, 0.72)', borderColor: 'rgba(148, 163, 184, 0.18)' }}>
            <Spin />
          </Card>
        ) : quality ? (
          <>
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(210px, 1fr))', gap: 12 }}>
              {metrics.map((item) => (
                <MetricCard key={item.label} {...item} />
              ))}
            </div>

            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(360px, 1fr))', gap: 12 }}>
              <Card
                title={<span style={{ color: '#e2e8f0' }}><SearchOutlined /> {t('rd.cacheRetrievalTitle', '缓存与召回')}</span>}
                style={{ background: 'rgba(15, 23, 42, 0.74)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
                styles={{ header: { borderBottomColor: 'rgba(148, 163, 184, 0.18)' } }}
              >
                <Space direction="vertical" size={12} style={{ width: '100%' }}>
                  <Text style={{ color: '#94a3b8' }}>
                    {t('rd.cacheRetrievalDesc', '缓存、Embedding、Symbol/Import 和依赖图只用于定位候选上下文；生成 Diff、Review 结论和行级判断仍必须读取真实文件核对。')}
                  </Text>
                  <Space wrap>
                    {cacheMetrics.map((item) => (
                      <Tag key={item.label} color={item.color}>{item.label}: {formatInteger(item.value)}</Tag>
                    ))}
                  </Space>
                </Space>
              </Card>

              <Card
                title={<span style={{ color: '#e2e8f0' }}><DatabaseOutlined /> {t('rd.embeddingIndexTitle', 'Embedding 索引')}</span>}
                style={{ background: 'rgba(15, 23, 42, 0.74)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
                styles={{ header: { borderBottomColor: 'rgba(148, 163, 184, 0.18)' } }}
              >
                <Space direction="vertical" size={12} style={{ width: '100%' }}>
                  <Space wrap>
                    <Tag color={quality.embeddingStoreEnabled ? 'green' : 'default'}>
                      {quality.embeddingStoreEnabled ? t('rd.embeddingStoreEnabled', 'SQLite 向量库已启用') : t('rd.embeddingStoreDisabled', 'SQLite 向量库未启用')}
                    </Tag>
                    {quality.embeddingModel ? <Tag color="blue">{quality.embeddingModel}</Tag> : null}
                    <Tag color="cyan">{t('rd.embeddingChunkCount', '向量块')}: {formatInteger(quality.embeddingChunkCount)}</Tag>
                  </Space>
                  <Space wrap>
                    {embeddingMetrics.map((item) => (
                      <Tag key={item.label}>{item.label}: {formatInteger(item.value)}</Tag>
                    ))}
                  </Space>
                  <Space wrap>
                    {summaryMetrics.map((item) => (
                      <Tag key={item.label} color={item.label.includes('LLM') ? 'gold' : 'default'}>{item.label}: {formatInteger(item.value)}</Tag>
                    ))}
                  </Space>
                </Space>
              </Card>

              <Card
                title={<span style={{ color: '#e2e8f0' }}><ApiOutlined /> {t('rd.tokenUsageTitle', 'Token 消耗')}</span>}
                style={{ background: 'rgba(15, 23, 42, 0.74)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
                styles={{ header: { borderBottomColor: 'rgba(148, 163, 184, 0.18)' } }}
              >
                <Space direction="vertical" size={12} style={{ width: '100%' }}>
                  <Tag color="geekblue">{t('rd.totalTokens', '总 Token')}: {formatInteger(quality.totalTokens)}</Tag>
                  <Space wrap>
                    {tokenMetrics.map((item) => (
                      <Tag key={item.label} color={item.color}>{item.label}: {formatInteger(item.value)}</Tag>
                    ))}
                  </Space>
                </Space>
              </Card>

              <Card
                title={<span style={{ color: '#e2e8f0' }}><FileTextOutlined /> {t('rd.toolReadTitle', '工具读取')}</span>}
                style={{ background: 'rgba(15, 23, 42, 0.74)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
                styles={{ header: { borderBottomColor: 'rgba(148, 163, 184, 0.18)' } }}
              >
                <Space direction="vertical" size={12} style={{ width: '100%' }}>
                  <Text style={{ color: '#94a3b8' }}>
                    {t('rd.toolReadDesc', '这里展示 runtime 实际读取/搜索次数，用于判断是否仍在盲扫。数值高不必然错误，但重复目标高说明还有可优化空间。')}
                  </Text>
                  <Space wrap>
                    {toolMetrics.map((item) => (
                      <Tag key={item.label} color={item.color}>{item.label}: {formatInteger(item.value)}</Tag>
                    ))}
                  </Space>
                </Space>
              </Card>
            </div>

            <Card
              title={<span style={{ color: '#e2e8f0' }}><BarChartOutlined /> {t('rd.qualityTotalsTitle', '质量汇总')}</span>}
              style={{ background: 'rgba(15, 23, 42, 0.74)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
              styles={{ header: { borderBottomColor: 'rgba(148, 163, 184, 0.18)' } }}
            >
              <Space direction="vertical" size={12} style={{ width: '100%' }}>
                <Text style={{ color: '#cbd5e1' }}>
                  {t('rd.qualityTotals', '任务 {{tasks}} · Diff {{diffs}} · 测试 {{tests}}', {
                    tasks: quality.taskCount,
                    diffs: quality.diffCount,
                    tests: quality.testRunCount,
                  })}
                </Text>
                <Space wrap>
                  <Tag color="success">{t('rd.statuses.completed', 'Completed')} {quality.completedCount}</Tag>
                  <Tag color="processing">{t('rd.statuses.running', 'Running')} {quality.runningCount}</Tag>
                  <Tag color="warning">{t('rd.statuses.waiting_approval', 'Waiting Approval')} {quality.waitingApprovalCount}</Tag>
                  <Tag color="error">{t('rd.statuses.failed', 'Failed')} {quality.failedCount}</Tag>
                  <Tag>{t('rd.statuses.cancelled', 'Cancelled')} {quality.cancelledCount}</Tag>
                </Space>
                {(quality.topFailedCommands ?? []).length > 0 ? (
                  <div>
                    <Text style={{ color: '#e2e8f0' }} strong>{t('rd.topFailedCommands', '高频失败命令')}</Text>
                    <Space direction="vertical" size={8} style={{ width: '100%', marginTop: 10 }}>
                      {(quality.topFailedCommands ?? []).map((item) => (
                        <div
                          key={item.command}
                          style={{
                            display: 'flex',
                            justifyContent: 'space-between',
                            gap: 12,
                            padding: '10px 12px',
                            borderRadius: 12,
                            background: 'rgba(2, 6, 23, 0.42)',
                            border: '1px solid rgba(148, 163, 184, 0.14)',
                          }}
                        >
                          <Text ellipsis style={{ color: '#cbd5e1', maxWidth: '80%' }}>{item.command}</Text>
                          <Tag color="error">{item.failureCount}</Tag>
                        </div>
                      ))}
                    </Space>
                  </div>
                ) : null}
              </Space>
            </Card>
          </>
        ) : (
          <Card style={{ background: 'rgba(15, 23, 42, 0.72)', borderColor: 'rgba(148, 163, 184, 0.18)' }}>
            <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={<span style={{ color: '#94a3b8' }}>{t('rd.noQualityData', '暂无质量数据')}</span>} />
          </Card>
        )}
      </Space>
    </div>
  );
}
