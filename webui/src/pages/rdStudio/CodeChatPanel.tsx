import { memo, useCallback, useEffect, useMemo, useState } from 'react';
import { Button, Card, Checkbox, Mentions, Space, Tag, Typography } from 'antd';
import { PlayCircleOutlined } from '@ant-design/icons';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { RD_FILE_MENTION_LIMIT } from './constants';
import type { CodeChatPanelProps, RdFileMentionCandidate } from './types';
import { buildFileMentionValue, repoLabel } from './utils';

const { Text, Title } = Typography;

export const CodeChatPanel = memo(function CodeChatPanel({
  canWrite,
  isPending,
  modelOptionsCount,
  selectedRepo,
  workspaceRepositories,
  selectedTask,
  model,
  deepModeEnabled,
  continueFromCurrentTask,
  selectedAgentProfile,
  selectedWorkflow,
  initialPrompt,
  onContinueFromCurrentTaskChange,
  onSubmit,
}: CodeChatPanelProps) {
  const { t } = useTranslation();
  const [draftPrompt, setDraftPrompt] = useState('');
  const [fileMentionSearch, setFileMentionSearch] = useState('');
  const [fileMentionActive, setFileMentionActive] = useState(false);

  useEffect(() => {
    const next = initialPrompt?.trim();
    if (next && !draftPrompt.trim()) {
      setDraftPrompt(next);
    }
  }, [draftPrompt, initialPrompt]);
  const workspaceRepoIds = useMemo(() => workspaceRepositories.map((repo) => repo.id), [workspaceRepositories]);
  const fileMentionQuery = useQuery({
    queryKey: [
      ...queryKeys.rd.repositoryFileSuggestions(workspaceRepoIds, fileMentionSearch, RD_FILE_MENTION_LIMIT),
      selectedRepo?.id ?? '',
    ],
    enabled: fileMentionActive && workspaceRepositories.length > 0,
    staleTime: 30_000,
    queryFn: async () => {
      const perRepoLimit = Math.max(8, Math.ceil(RD_FILE_MENTION_LIMIT / Math.max(workspaceRepositories.length, 1)));
      const groups = await Promise.all(workspaceRepositories.map(async (repo) => {
        try {
          const files = await rdApi.repositoryFileSuggestions(repo.id, {
            q: fileMentionSearch,
            limit: perRepoLimit,
          });
          return files.map((file): RdFileMentionCandidate => ({
            ...file,
            repositoryId: repo.id,
            repositoryName: repo.name,
            repositoryBranch: repo.branch,
            mentionValue: buildFileMentionValue(repo, file, selectedRepo?.id),
            isPrimaryRepository: repo.id === selectedRepo?.id,
          }));
        } catch {
          return [] as RdFileMentionCandidate[];
        }
      }));
      return groups
        .flat()
        .sort((left, right) => {
          if (left.isPrimaryRepository !== right.isPrimaryRepository) {
            return left.isPrimaryRepository ? -1 : 1;
          }
          return left.path.length - right.path.length || left.path.localeCompare(right.path);
        })
        .slice(0, RD_FILE_MENTION_LIMIT);
    },
  });
  const fileMentionOptions = useMemo(
    () => (fileMentionQuery.data ?? []).map((candidate) => ({
      value: candidate.mentionValue,
      label: (
        <span className="rd-file-mention-option">
          <span className="rd-file-mention-path">{candidate.path}</span>
          <span className="rd-file-mention-meta">
            {candidate.repositoryName} · {candidate.repositoryBranch}
            {candidate.language ? ` · ${candidate.language}` : ''}
          </span>
        </span>
      ),
    })),
    [fileMentionQuery.data],
  );
  const submitDisabled = !canWrite || !draftPrompt.trim() || modelOptionsCount === 0 || isPending || (continueFromCurrentTask && !selectedTask);
  const demandExamples = useMemo(
    () => [
      {
        title: t('rd.exampleStartTitle', '了解项目'),
        prompt: t('rd.exampleStartPrompt', '这个项目怎么启动？请读取 README、package/cargo 配置和主要入口，给我一份最短可执行启动说明。'),
      },
      {
        title: t('rd.exampleFixTitle', '修复问题'),
        prompt: t('rd.exampleFixPrompt', '修复一个我会描述的 bug：请先定位相关文件并生成计划，再给出可审查 Diff，不要直接覆盖文件。'),
      },
      {
        title: t('rd.exampleReviewTitle', '代码审查'),
        prompt: t('rd.exampleReviewPrompt', '请对当前仓库做一轮代码审查，优先找真实 bug、回归风险和缺失测试，按严重级别输出。'),
      },
      {
        title: t('rd.exampleErrorTitle', '解释报错'),
        prompt: t('rd.exampleErrorPrompt', '我遇到下面这个报错，请结合项目代码解释原因并给出修复方案：\n\n'),
      },
    ],
    [t],
  );

  const handleSubmit = useCallback(() => {
    const prompt = draftPrompt.trim();
    if (!prompt) return;
    void onSubmit(prompt)
      .then(() => setDraftPrompt(''))
      .catch(() => undefined);
  }, [draftPrompt, onSubmit]);

  return (
    <>
      <Card className="rd-code-composer-card" styles={{ body: { padding: 12 } }}>
        <Space direction="vertical" size={10} style={{ width: '100%' }}>
          <Space wrap style={{ justifyContent: 'space-between', width: '100%', alignItems: 'center' }}>
            <Space direction="vertical" size={2}>
              <Text className="rd-code-composer-eyebrow">
                {t('rd.requirementComposerEyebrow', 'Requirement Composer')}
              </Text>
              <Title level={5} className="rd-code-composer-title">
                {t('rd.requirementComposerTitle', '今天要让代码库帮你完成什么？')}
              </Title>
            </Space>
            <Space direction="vertical" size={4} style={{ alignItems: 'flex-end' }}>
              <Tag color="cyan">{t('rd.codeMode', 'Code')}</Tag>
              {deepModeEnabled ? <Tag color="volcano">{t('rd.deepMode', '深度模式')}</Tag> : null}
              <Text style={{ color: '#64748b', fontSize: 12 }}>
                {t('rd.codeModeRouteHint', '直接描述目标，AOS 会在内部判断是问答、改代码、解释还是审查。')}
              </Text>
            </Space>
          </Space>

          <Mentions
            prefix="@"
            split=" "
            value={draftPrompt}
            onChange={setDraftPrompt}
            onSearch={(value) => {
              setFileMentionActive(true);
              setFileMentionSearch(value.trim());
            }}
            placeholder={t('rd.promptPlaceholder', '例如：这个项目怎么启动？或者：修复登录失败时没有错误提示的问题，并给出 diff。')}
            autoSize={{ minRows: 5, maxRows: 10 }}
            options={fileMentionOptions}
            className="rd-requirement-mentions"
            style={{ background: '#07111f' }}
          />
          <Text style={{ color: '#64748b', fontSize: 12 }}>
            {t('rd.fileMentionHint', '输入 @ 可引用工作区文件；主执行仓库的 @文件 会自动作为优先上下文读取。')}
            {fileMentionQuery.isFetching ? ` · ${t('common.loading', '加载中')}` : ''}
          </Text>

          {selectedTask ? (
            <div
              style={{
                padding: '10px 12px',
                borderRadius: 14,
                background: continueFromCurrentTask ? 'rgba(20, 184, 166, 0.12)' : 'rgba(15, 23, 42, 0.42)',
                border: continueFromCurrentTask ? '1px solid rgba(45, 212, 191, 0.42)' : '1px solid rgba(148, 163, 184, 0.16)',
              }}
            >
              <Checkbox
                checked={continueFromCurrentTask}
                disabled={isPending}
                onChange={(event) => onContinueFromCurrentTaskChange(event.target.checked)}
              >
                <span style={{ color: '#dbeafe' }}>{t('rd.followUpFromCurrentTask', '基于当前任务继续')}</span>
              </Checkbox>
              <div style={{ color: '#94a3b8', fontSize: 12, marginTop: 4, paddingLeft: 24 }}>
                {t('rd.followUpFromCurrentTaskHint', '会携带上一轮计划、总结、Diff 与测试摘要，继续走研发 runtime。')}
              </div>
            </div>
          ) : null}

          <Space wrap style={{ justifyContent: 'space-between', width: '100%' }}>
            <Space wrap>
              <Tag color={selectedRepo ? 'cyan' : 'default'}>{selectedRepo ? repoLabel(selectedRepo) : t('rd.noRepoSelected', '未选择仓库')}</Tag>
              {workspaceRepositories.length > 1 ? (
                <Tag color="geekblue">{t('rd.workspaceRepoCount', '工作区 {{count}} 个项目', { count: workspaceRepositories.length })}</Tag>
              ) : null}
              {deepModeEnabled ? <Tag color="volcano">{t('rd.deepMode', '深度模式')}</Tag> : null}
              <Tag color={model ? 'blue' : 'default'}>{model || t('common.na')}</Tag>
              {selectedAgentProfile ? <Tag color="gold">{selectedAgentProfile.name}</Tag> : null}
              {selectedWorkflow ? <Tag color="purple">{selectedWorkflow.name}</Tag> : null}
            </Space>
            <Button
              type="primary"
              icon={<PlayCircleOutlined />}
              disabled={submitDisabled}
              loading={isPending}
              onClick={handleSubmit}
            >
              {continueFromCurrentTask ? t('rd.createFollowUpTask', '继续处理') : t('rd.createTask', '生成计划 / Diff')}
            </Button>
          </Space>
        </Space>
      </Card>

      {!selectedTask ? (
        <Card style={{ background: 'rgba(15, 23, 42, 0.62)', borderColor: 'rgba(148, 163, 184, 0.16)' }}>
          <Space direction="vertical" size={14} style={{ width: '100%' }}>
            <Space direction="vertical" size={2}>
              <Text style={{ color: '#e2e8f0', fontWeight: 700 }}>{t('rd.examplesTitle', '不知道怎么开始？可以直接改这些例子')}</Text>
              <Text style={{ color: '#94a3b8' }}>{t('rd.examplesDesc', '自然语言越像你平时给同事提需求，Agent 越容易理解真实目标。')}</Text>
            </Space>
            <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(210px, 1fr))', gap: 10 }}>
              {demandExamples.map((item) => (
                <Button
                  key={item.title}
                  type="text"
                  onClick={() => {
                    setDraftPrompt(item.prompt);
                  }}
                  style={{
                    height: 'auto',
                    minHeight: 86,
                    padding: 14,
                    textAlign: 'left',
                    whiteSpace: 'normal',
                    border: '1px solid rgba(148, 163, 184, 0.16)',
                    background: 'rgba(2, 6, 23, 0.34)',
                    color: '#dbeafe',
                  }}
                >
                  <Space direction="vertical" size={6} style={{ width: '100%' }}>
                    <Text style={{ color: '#f8fafc' }} strong>{item.title}</Text>
                    <Text style={{ color: '#94a3b8', fontSize: 12 }}>{item.prompt.slice(0, 68)}...</Text>
                  </Space>
                </Button>
              ))}
            </div>
          </Space>
        </Card>
      ) : null}
    </>
  );
});
