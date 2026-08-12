import { useEffect, useMemo, useState } from 'react';
import {
  Alert,
  Button,
  Card,
  Drawer,
  Empty,
  Form,
  Input,
  Select,
  Space,
  Table,
  Tag,
  Typography,
  message,
  Popconfirm,
} from 'antd';
import { CheckCircleOutlined, CopyOutlined, DeleteOutlined, DownloadOutlined, EditOutlined, FileTextOutlined, PlayCircleOutlined, PlusOutlined, SyncOutlined } from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import dayjs from 'dayjs';
import { apiKeysApi, rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { Markdown } from '@/components/chat';
import type { ApiKeyRecord, RdSpec } from '@/types';

const { Text, Title, Paragraph } = Typography;

function isRdModel(key: ApiKeyRecord): boolean {
  if (!key.enabled || key.model_type !== 'chat') return false;
  if (key.runtime_available === false) return false;
  const scenarios = key.scenarios;
  return !scenarios || scenarios.length === 0 || scenarios.includes('rd') || scenarios.includes('agent');
}

function modelOptions(keys: ApiKeyRecord[]) {
  const seen = new Set<string>();
  return keys
    .filter(isRdModel)
    .filter((key) => {
      const value = key.model || key.name;
      if (seen.has(value)) return false;
      seen.add(value);
      return true;
    })
    .sort((a, b) => (a.priority ?? 100) - (b.priority ?? 100))
    .map((key) => ({ value: key.model || key.name, label: `${key.model || key.name} · ${key.provider}` }));
}

function SpecSection({ title, children, emptyText }: { title: string; children?: string | null; emptyText: string }) {
  return (
    <Card size="small" title={title}>
      {children?.trim() ? <Markdown>{children}</Markdown> : <Text type="secondary">{emptyText}</Text>}
    </Card>
  );
}

export default function RdSpecs() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [selectedSpecId, setSelectedSpecId] = useState<string | undefined>();
  const [revisionFeedback, setRevisionFeedback] = useState('');
  const [form] = Form.useForm();

  const specsQuery = useQuery({
    queryKey: queryKeys.rd.specs(),
    queryFn: rdApi.listSpecs,
    refetchInterval: (query) => (query.state.data ?? []).some((spec) => ['queued', 'running'].includes(spec.status)) ? 3000 : false,
  });
  const repositoriesQuery = useQuery({ queryKey: queryKeys.rd.repositories(), queryFn: rdApi.listRepositories });
  const apiKeysQuery = useQuery({ queryKey: queryKeys.apiKeys.list(), queryFn: apiKeysApi.list });

  const models = useMemo(() => modelOptions(apiKeysQuery.data?.keys ?? []), [apiKeysQuery.data?.keys]);
  const repositories = repositoriesQuery.data?.repositories ?? [];
  const specs = specsQuery.data ?? [];
  const selectedSpec = specs.find((spec) => spec.id === selectedSpecId) ?? null;
  const repoNameMap = useMemo(() => new Map(repositories.map((repo) => [repo.id, repo.name])), [repositories]);
  const statusLabel = (value?: string | null) => {
    const raw = value?.trim();
    if (!raw) return '';
    const key = raw.toLowerCase();
    return t(`rd.statuses.${key}`, { defaultValue: raw });
  };
  const statusColor = (value?: string | null) => {
    switch (value?.toLowerCase()) {
      case 'completed': return 'success';
      case 'failed': return 'error';
      case 'running': return 'processing';
      case 'queued': return 'warning';
      default: return 'default';
    }
  };

  const createMutation = useMutation({
    mutationFn: (values: { repositoryId?: string; repositoryIds?: string[]; title?: string; prompt: string; model?: string }) => rdApi.createSpec(values),
    onSuccess: (spec) => {
      message.success(t('rd.specCreated', '计划已保存，核心设计将在后台生成'));
      setDrawerOpen(false);
      setSelectedSpecId(spec.id);
      form.resetFields();
      queryClient.invalidateQueries({ queryKey: queryKeys.rd.specs() });
    },
    onError: (error: Error) => message.error(error.message || t('rd.specCreateFailed', '规格生成失败')),
  });

  const deleteMutation = useMutation({
    mutationFn: rdApi.deleteSpec,
    onSuccess: async (_, deletedId) => {
      if (selectedSpecId === deletedId) setSelectedSpecId(undefined);
      message.success(t('rd.planDeleted', '计划已删除'));
      await queryClient.invalidateQueries({ queryKey: queryKeys.rd.specs() });
    },
    onError: (error: Error) => message.error(error.message || t('rd.planDeleteFailed', '删除计划失败')),
  });

  const stageMutation = useMutation({
    mutationFn: async ({ id, action }: { id: string; action: 'generateDesign' | 'approveDesign' | 'generateTasks' | 'approveTasks' }) => {
      switch (action) {
        case 'generateDesign': return rdApi.generateDesign(id);
        case 'approveDesign': return rdApi.approveDesign(id);
        case 'generateTasks': return rdApi.generateTasks(id);
        case 'approveTasks': return rdApi.approveTasks(id);
      }
    },
    onSuccess: async (spec) => {
      message.success(t('rd.planStageCompleted', '阶段操作已完成'));
      await queryClient.invalidateQueries({ queryKey: queryKeys.rd.specs() });
      setSelectedSpecId(spec.id);
    },
    onError: (error: Error) => message.error(error.message || t('rd.planStageActionFailed', '阶段操作失败')),
  });

  const revisionMutation = useMutation({
    mutationFn: ({ id, stage, feedback }: { id: string; stage: 'spec' | 'design' | 'tasks'; feedback: string }) =>
      rdApi.reviseSpecStage(id, { stage, feedback }),
    onSuccess: async (spec) => {
      setRevisionFeedback('');
      message.success(t('rd.planRevisionQueued', '修改意见已提交，AI 正在后台修订当前文档'));
      await queryClient.invalidateQueries({ queryKey: queryKeys.rd.specs() });
      setSelectedSpecId(spec.id);
    },
    onError: (error: Error) => message.error(error.message || t('rd.planRevisionFailed', '提交修改意见失败')),
  });

  const nextStageAction = (spec: RdSpec) => {
    if (['queued', 'running'].includes(spec.status)) return null;
    if (spec.requirementsMd?.trim() && !spec.approvedRequirementsAt) {
      return { action: 'generateDesign' as const, label: t('rd.generateDesign', '生成代码研发方案') };
    }
    if (spec.approvedRequirementsAt && !spec.designMd?.trim()) {
      return { action: 'generateDesign' as const, label: t('rd.generateDesign', '生成代码研发方案') };
    }
    if (spec.designMd?.trim() && !spec.approvedDesignAt) {
      return { action: 'approveDesign' as const, label: t('rd.approveDesign', '确认代码研发方案') };
    }
    if (spec.approvedDesignAt && !spec.tasksMd?.trim()) {
      return { action: 'generateTasks' as const, label: t('rd.generateTasks', '生成 Tasks') };
    }
    if (spec.tasksMd?.trim() && !spec.approvedTasksAt) {
      return { action: 'approveTasks' as const, label: t('rd.approveTasks', '确认 Tasks') };
    }
    return null;
  };

  const revisionStage = (spec: RdSpec): 'spec' | 'design' | 'tasks' | null => {
    if (spec.tasksMd?.trim()) return 'tasks';
    if (spec.designMd?.trim()) return 'design';
    if (spec.requirementsMd?.trim()) return 'spec';
    return null;
  };

  useEffect(() => {
    setRevisionFeedback('');
  }, [selectedSpecId]);

  const downloadSpecPackage = (spec: RdSpec) => {
    const sections = [
      `# ${spec.title}`,
      `\n## 原始需求\n\n${spec.prompt}`,
      `\n## 需求方案\n\n${spec.requirementsMd || '待生成'}`,
      `\n## 代码研发方案\n\n${spec.designMd || '待生成'}`,
      `\n## 任务拆解\n\n${spec.tasksMd || '待生成'}`,
      `\n## 验收标准\n\n${spec.acceptanceMd || '待生成'}`,
    ].join('\n');
    const url = URL.createObjectURL(new Blob([sections], { type: 'text/markdown;charset=utf-8' }));
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `${spec.title.replace(/[^\p{L}\p{N}._-]+/gu, '-').slice(0, 80) || 'aos-engineering-plan'}.md`;
    anchor.click();
    URL.revokeObjectURL(url);
    message.success(t('rd.specPackageDownloaded', '方案已下载，可交给 Codex、Kiro 或 Claude Code 执行'));
  };

  const copyHandoffPrompt = async (spec: RdSpec) => {
    const prompt = `请按 AOS 研发方案执行：\n\n${spec.title}\n\n${spec.designMd || spec.requirementsMd || spec.prompt}\n\n先审查方案与仓库现状，再按任务清单逐项实施，完成后运行验收标准中的测试。`;
    await navigator.clipboard?.writeText(prompt);
    message.success(t('rd.handoffCopied', '外部 Code Agent 交接提示词已复制'));
  };

  const columns: ColumnsType<RdSpec> = [
    {
      title: t('rd.specTitle', '规格'),
      dataIndex: 'title',
      width: 280,
      render: (title: string, record) => (
        <Space direction="vertical" size={0}>
          <Text strong>{title}</Text>
          <Text type="secondary" style={{ fontSize: 12 }}>{record.prompt.slice(0, 100)}</Text>
        </Space>
      ),
    },
    {
      title: t('rd.repository', '仓库'),
      dataIndex: 'repositoryId',
      width: 180,
      render: (_id: string | null | undefined, record) => {
        const ids = record.repositoryIds?.length ? record.repositoryIds : (record.repositoryId ? [record.repositoryId] : []);
        if (ids.length === 0) return t('common.na');
        return <Space size={4} wrap>{ids.map((id) => <Tag key={id}>{repoNameMap.get(id) ?? id}</Tag>)}</Space>;
      },
    },
    {
      title: t('common.status'),
      dataIndex: 'status',
      width: 120,
      render: (value: string, record) => (
        <Space size={4} wrap>
          <Tag color={statusColor(value)} icon={value === 'running' || value === 'queued' ? <SyncOutlined spin /> : undefined}>{statusLabel(value)}</Tag>
          <Text type="secondary" style={{ fontSize: 12 }}>{statusLabel(record.currentStage)}</Text>
        </Space>
      ),
    },
    {
      title: t('common.model'),
      dataIndex: 'model',
      width: 180,
      render: (value?: string | null) => value || t('common.na'),
    },
    {
      title: t('common.createdAt'),
      dataIndex: 'createdAt',
      width: 180,
      render: (value: string) => dayjs(value).format('YYYY-MM-DD HH:mm:ss'),
    },
    {
      title: t('common.actions'),
      key: 'actions',
      fixed: 'right',
      width: 260,
      render: (_, record) => (
        <Space>
          <Button size="small" onClick={() => setSelectedSpecId(record.id)}>{t('common.viewDetail')}</Button>
          <Button size="small" icon={<DownloadOutlined />} onClick={() => downloadSpecPackage(record)}>{t('rd.downloadPlan', '下载方案')}</Button>
          <Popconfirm
            title={t('rd.planDeleteConfirm', '确认删除该研发计划？')}
            onConfirm={() => deleteMutation.mutate(record.id)}
            okText={t('common.delete')}
            cancelText={t('common.cancel')}
            disabled={['queued', 'running'].includes(record.status)}
          >
            <Button
              size="small"
              danger
              icon={<DeleteOutlined />}
              disabled={['queued', 'running'].includes(record.status)}
              loading={deleteMutation.isPending && deleteMutation.variables === record.id}
              aria-label={t('common.delete')}
            />
          </Popconfirm>
        </Space>
      ),
    },
  ];

  return (
    <div style={{ padding: 24 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', gap: 16, marginBottom: 16 }}>
        <div>
          <Title level={4} style={{ margin: 0 }}>{t('rd.specsTitle', '研发方案设计')}</Title>
          <Text type="secondary">{t('rd.specsSubtitle', '从自然语言与真实代码仓库生成可审查、可下载、可交给外部 Code Agent 执行的需求与研发方案。')}</Text>
        </div>
        <Space>
          <Button icon={<SyncOutlined />} loading={specsQuery.isFetching} onClick={() => specsQuery.refetch()}>{t('common.refresh')}</Button>
          <Button type="primary" icon={<PlusOutlined />} onClick={() => setDrawerOpen(true)}>{t('rd.newSpec', '新建计划')}</Button>
        </Space>
      </div>

      {models.length === 0 ? (
        <Alert style={{ marginBottom: 16 }} type="warning" showIcon message={t('rd.noModelTitle', '未配置研发聊天模型')} description={t('rd.noModelDesc', '请到 API 密钥管理添加适用场景为研发、类型为聊天模型的可用密钥。')} />
      ) : null}

      <Card styles={{ body: { padding: 0 } }}>
        <Table
          rowKey="id"
          columns={columns}
          dataSource={specs}
          loading={specsQuery.isLoading}
          scroll={{ x: 'max-content' }}
          pagination={{ pageSize: 20, size: 'small' }}
          locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.emptySpecs', '暂无规格文档')} /> }}
        />
      </Card>

      <Drawer title={t('rd.newSpec', '新建计划')} open={drawerOpen} onClose={() => setDrawerOpen(false)} width={620} footer={
        <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
          <Button onClick={() => setDrawerOpen(false)}>{t('common.cancel')}</Button>
          <Button type="primary" icon={<PlayCircleOutlined />} loading={createMutation.isPending} disabled={models.length === 0} onClick={async () => {
            const values = await form.validateFields();
            createMutation.mutate(values);
          }}>{t('rd.generateSpec', '生成计划')}</Button>
        </Space>
      }>
        <Form form={form} layout="vertical" requiredMark="optional" initialValues={{ model: models[0]?.value }}>
          <Form.Item name="repositoryIds" label={t('rd.repositories', '参与仓库')} extra={t('rd.repositoriesExtra', '可选择多个相互调用的服务，AI 会分别读取并标注各仓库的代码证据。')}>
            <Select mode="multiple" allowClear showSearch optionFilterProp="label" options={repositories.map((repo) => ({ value: repo.id, label: `${repo.name} · ${repo.branch}` }))} placeholder={t('rd.noRepoSelected', '未选择仓库')} />
          </Form.Item>
          <Form.Item name="model" label={t('common.model')} rules={[{ required: true, message: t('common.required') }]}>
            <Select options={models} placeholder={t('rd.selectModel', '选择研发模型')} />
          </Form.Item>
          <Form.Item name="title" label={t('rd.specTitle', '规格')}>
            <Input placeholder={t('rd.specTitlePlaceholder', '例如：登录错误提示优化')} />
          </Form.Item>
          <Form.Item name="prompt" label={t('rd.requirement', '需求')} rules={[{ required: true, message: t('common.required') }]}>
            <Input.TextArea autoSize={{ minRows: 8, maxRows: 14 }} placeholder={t('rd.specPromptPlaceholder', '用自然语言描述需求、目标用户、边界、验收方式。AI 会整理成需求/设计/任务/验收四段。')} />
          </Form.Item>
        </Form>
      </Drawer>

      <Drawer
        title={selectedSpec ? <Space><FileTextOutlined /> {selectedSpec.title}</Space> : t('common.detail')}
        open={!!selectedSpecId}
        onClose={() => setSelectedSpecId(undefined)}
        width={900}
        extra={selectedSpec ? (
          <Space>
            <Popconfirm
              title={t('rd.planDeleteConfirm', '确认删除该研发计划？')}
              onConfirm={() => deleteMutation.mutate(selectedSpec.id)}
              okText={t('common.delete')}
              cancelText={t('common.cancel')}
              disabled={['queued', 'running'].includes(selectedSpec.status)}
            >
              <Button danger icon={<DeleteOutlined />} disabled={['queued', 'running'].includes(selectedSpec.status)}>{t('common.delete')}</Button>
            </Popconfirm>
            <Button icon={<CopyOutlined />} onClick={() => void copyHandoffPrompt(selectedSpec)}>{t('rd.copyHandoff', '复制交接提示词')}</Button>
            <Button type="primary" icon={<DownloadOutlined />} onClick={() => downloadSpecPackage(selectedSpec)}>{t('rd.downloadPlan', '下载方案')}</Button>
          </Space>
        ) : null}
      >
        {!selectedSpec ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} /> : (
          <Space direction="vertical" size={16} style={{ width: '100%' }}>
            <Card size="small" title={t('rd.originalRequirement', '原始需求')}>
              <Paragraph>{selectedSpec.prompt}</Paragraph>
            </Card>
            {['queued', 'running'].includes(selectedSpec.status) ? (
              <Alert
                type="info"
                showIcon
                icon={<SyncOutlined spin />}
                message={t('rd.planRunning', '计划正在生成')}
                description={t('rd.planRunningDesc', '已保存到计划列表，当前阶段：{{stage}}。页面会自动刷新，生成完成前可以离开此页面。', { stage: statusLabel(selectedSpec.currentStage) || selectedSpec.currentStage || 'spec' })}
              />
            ) : null}
            {selectedSpec.lastError ? <Alert type="error" showIcon message={t('rd.planFailed', '计划生成失败')} description={selectedSpec.lastError} /> : null}
            {nextStageAction(selectedSpec) ? (
              <Alert
                type="info"
                showIcon
                message={t('rd.planAwaitingNextStage', '当前阶段已完成，可继续生成下一阶段')}
                description={t('rd.planAwaitingNextStageDesc', '请先审查当前文档；如有偏差，可在下方填写修改意见让 AI 修订，确认内容合适后再生成下一阶段。')}
                action={(() => {
                  const next = nextStageAction(selectedSpec)!;
                  return (
                    <Button
                      type="primary"
                      icon={<CheckCircleOutlined />}
                      loading={stageMutation.isPending}
                      onClick={() => stageMutation.mutate({ id: selectedSpec.id, action: next.action })}
                    >
                      {next.label}
                    </Button>
                  );
                })()}
              />
            ) : null}
            {revisionStage(selectedSpec) && !['queued', 'running'].includes(selectedSpec.status) ? (
              <Card size="small" title={t('rd.planRevisionTitle', '让 AI 修改当前文档')}>
                <Space direction="vertical" size={10} style={{ width: '100%' }}>
                  <Text type="secondary">
                    {t('rd.planRevisionDesc', '说明哪些内容不符合需求、需要补充哪些约束或代码证据。修订会保留历史版本，并使依赖当前文档的后续阶段重新生成。')}
                  </Text>
                  <Input.TextArea
                    value={revisionFeedback}
                    onChange={(event) => setRevisionFeedback(event.target.value)}
                    autoSize={{ minRows: 3, maxRows: 8 }}
                    maxLength={4000}
                    showCount
                    placeholder={t('rd.planRevisionPlaceholder', '例如：补充灰度发布和回滚方案；说明三个微服务的调用顺序，并引用真实文件路径。')}
                  />
                  <Button
                    icon={<EditOutlined />}
                    loading={revisionMutation.isPending}
                    disabled={!revisionFeedback.trim()}
                    onClick={() => revisionMutation.mutate({
                      id: selectedSpec.id,
                      stage: revisionStage(selectedSpec)!,
                      feedback: revisionFeedback.trim(),
                    })}
                  >
                    {t('rd.planReviseAction', '按修改意见重新生成当前文档')}
                  </Button>
                </Space>
              </Card>
            ) : null}
            <SpecSection title={t('rd.requirementsDoc', '需求文档')} children={selectedSpec.requirementsMd} emptyText={t('common.noData')} />
            <SpecSection title={t('rd.designDoc', '技术设计')} children={selectedSpec.designMd} emptyText={t('common.noData')} />
            <SpecSection title={t('rd.tasksDoc', '任务清单')} children={selectedSpec.tasksMd} emptyText={t('common.noData')} />
            <SpecSection title={t('rd.acceptanceDoc', '验收标准')} children={selectedSpec.acceptanceMd} emptyText={t('common.noData')} />
          </Space>
        )}
      </Drawer>
    </div>
  );
}
