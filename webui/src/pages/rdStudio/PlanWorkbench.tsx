import { Alert, Button, Empty, Input, Popconfirm, Select, Space, Spin, Tabs, Tag, Typography, message } from 'antd';
import { CheckCircleOutlined, DeleteOutlined, FileTextOutlined, PlayCircleOutlined, ReloadOutlined } from '@ant-design/icons';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { rdApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { RdRepository, RdSpec, RdSpecTaskItem } from '@/types';
import { PlanStageStepper } from './PlanStageStepper';
import { SpecEditor } from './SpecEditor';
import { TaskItemBoard } from './TaskItemBoard';

const { Text, Title } = Typography;

function canGenerateDesign(spec?: RdSpec | null) {
  return !!spec?.approvedRequirementsAt;
}

function canGenerateTasks(spec?: RdSpec | null) {
  return !!spec?.approvedDesignAt;
}

function canImplement(spec?: RdSpec | null) {
  return !!spec?.approvedTasksAt && (spec.taskItems?.length ?? 0) > 0;
}

export function PlanWorkbench({
  repositories,
  selectedRepoId,
  model,
  agentProfileId,
  workflowId,
  onSelectRepo,
  onOpenTask,
}: {
  repositories: RdRepository[];
  selectedRepoId?: string;
  model?: string;
  agentProfileId?: string;
  workflowId?: string;
  onSelectRepo: (id?: string) => void;
  onOpenTask: (taskId: string) => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [selectedSpecId, setSelectedSpecId] = useState<string | undefined>();
  const [prompt, setPrompt] = useState('');
  const [title, setTitle] = useState('');
  const [planRepositoryIds, setPlanRepositoryIds] = useState<string[]>(selectedRepoId ? [selectedRepoId] : []);
  const [draftSpec, setDraftSpec] = useState<RdSpec | null>(null);
  const [implementingTaskId, setImplementingTaskId] = useState<string | null>(null);

  const specsQuery = useQuery({
    queryKey: queryKeys.rd.specs(),
    queryFn: rdApi.listSpecs,
    refetchInterval: (query) => (query.state.data ?? []).some((spec) =>
      ['queued', 'running'].includes(spec.status) || spec.currentStage === 'implementation'
    ) ? 3000 : false,
  });
  const specs = specsQuery.data ?? [];

  useEffect(() => {
    if (!selectedSpecId && specs.length > 0) {
      setSelectedSpecId(specs[0].id);
    }
  }, [selectedSpecId, specs]);

  const specQuery = useQuery({
    queryKey: queryKeys.rd.spec(selectedSpecId),
    queryFn: () => rdApi.getSpec(selectedSpecId!),
    enabled: !!selectedSpecId,
    refetchInterval: (query) => {
      const spec = query.state.data;
      return spec && (
        ['queued', 'running'].includes(spec.status) ||
        spec.taskItems?.some((item) => ['running', 'waiting_approval'].includes(item.status))
      ) ? 3000 : false;
    },
  });
  const selectedSpec = draftSpec?.id === selectedSpecId ? draftSpec : specQuery.data ?? null;

  const specEventsQuery = useQuery({
    queryKey: queryKeys.rd.specEvents(selectedSpecId),
    queryFn: () => rdApi.specEvents(selectedSpecId!),
    enabled: !!selectedSpecId,
  });

  const invalidateSpec = async (id?: string) => {
    await queryClient.invalidateQueries({ queryKey: queryKeys.rd.specs() });
    if (id) {
      await queryClient.invalidateQueries({ queryKey: queryKeys.rd.spec(id) });
      await queryClient.invalidateQueries({ queryKey: queryKeys.rd.specEvents(id) });
    }
  };

  const createSpecMutation = useMutation({
    mutationFn: () => rdApi.createSpec({
      repositoryIds: planRepositoryIds,
      repositoryId: planRepositoryIds[0],
      title: title.trim() || undefined,
      prompt: prompt.trim(),
      model,
      mode: 'plan',
    }),
    onSuccess: async (spec) => {
      message.success(t('rd.planSpecCreated', 'Spec 已创建'));
      setPrompt('');
      setTitle('');
      setSelectedSpecId(spec.id);
      // Initial generation runs in the background. Keep the server query as
      // the source of truth so queued/running/completed transitions can poll.
      setDraftSpec(null);
      await invalidateSpec(spec.id);
    },
    onError: (error: Error) => message.error(error.message || t('rd.planSpecCreateFailed', '创建计划失败')),
  });

  const updateSpecMutation = useMutation({
    mutationFn: (data: Partial<RdSpec>) => rdApi.updateSpec(selectedSpecId!, {
      title: data.title ?? undefined,
      requirementsMd: data.requirementsMd ?? undefined,
      designMd: data.designMd ?? undefined,
      tasksMd: data.tasksMd ?? undefined,
      acceptanceMd: data.acceptanceMd ?? undefined,
      taskItems: data.taskItems,
    }),
    onSuccess: async (spec) => {
      setDraftSpec(spec);
      await invalidateSpec(spec.id);
    },
    onError: (error: Error) => message.error(error.message || t('rd.planSpecSaveFailed', '保存失败')),
  });

  const stageMutation = useMutation({
    mutationFn: async (action: string) => {
      if (!selectedSpecId) throw new Error('spec not selected');
      switch (action) {
        case 'generateSpec': return rdApi.generateSpec(selectedSpecId);
        case 'approveSpec': return rdApi.approveSpec(selectedSpecId);
        case 'generateDesign': return rdApi.generateDesign(selectedSpecId);
        case 'approveDesign': return rdApi.approveDesign(selectedSpecId);
        case 'generateTasks': return rdApi.generateTasks(selectedSpecId);
        case 'approveTasks': return rdApi.approveTasks(selectedSpecId);
        case 'finalReport': return rdApi.finalReportSpec(selectedSpecId);
        default: throw new Error(`unknown action: ${action}`);
      }
    },
    onSuccess: async (spec) => {
      setDraftSpec(spec);
      await invalidateSpec(spec.id);
    },
    onError: (error: Error) => message.error(error.message || t('rd.planStageActionFailed', '阶段操作失败')),
  });

  const deleteSpecMutation = useMutation({
    mutationFn: rdApi.deleteSpec,
    onSuccess: async (_, deletedId) => {
      if (selectedSpecId === deletedId) {
        setSelectedSpecId(undefined);
        setDraftSpec(null);
      }
      message.success(t('rd.planDeleted', '计划已删除'));
      await invalidateSpec();
    },
    onError: (error: Error) => message.error(error.message || t('rd.planDeleteFailed', '删除计划失败')),
  });

  const implementTaskMutation = useMutation({
    mutationFn: (item: RdSpecTaskItem) => {
      setImplementingTaskId(item.id);
      return rdApi.implementSpecTask(selectedSpecId!, {
        taskItemId: item.id,
        model,
        agentProfileId,
        workflowId,
      });
    },
    onSuccess: async (task) => {
      message.success(t('rd.planImplementationStarted', '已创建真实 RD 任务'));
      setImplementingTaskId(null);
      await invalidateSpec(selectedSpecId);
      await queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks() });
      onOpenTask(task.id);
    },
    onError: (error: Error) => {
      setImplementingTaskId(null);
      message.error(error.message || t('rd.planImplementationFailed', '执行任务项失败'));
    },
  });

  const implementAllMutation = useMutation({
    mutationFn: () => rdApi.implementAllSpecTasks(selectedSpecId!, { model, agentProfileId, workflowId }),
    onSuccess: async (tasks) => {
      message.success(t('rd.planImplementationAllStarted', '已创建 {{count}} 个 RD 任务', { count: tasks.length }));
      await invalidateSpec(selectedSpecId);
      await queryClient.invalidateQueries({ queryKey: queryKeys.rd.tasks() });
      if (tasks[0]) onOpenTask(tasks[0].id);
    },
    onError: (error: Error) => message.error(error.message || t('rd.planImplementationFailed', '执行任务项失败')),
  });

  const taskItems = useMemo(() => selectedSpec?.taskItems ?? [], [selectedSpec?.taskItems]);

  return (
    <div className="rd-plan-workbench">
      <aside className="rd-plan-list">
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          <div>
            <Text type="secondary">{t('rd.repositories', '代码仓库')}</Text>
            <Select
              allowClear
              value={selectedRepoId}
              onChange={onSelectRepo}
              style={{ width: '100%', marginTop: 6 }}
              placeholder={t('rd.primaryRepositoryPlaceholder', '选择 Diff / 测试执行仓库')}
              options={repositories.map((repo) => ({ value: repo.id, label: `${repo.name} · ${repo.branch}` }))}
            />
          </div>
          <div className="rd-plan-create">
            <Text strong>{t('rd.createPlan', '创建计划')}</Text>
            <Select
              mode="multiple"
              allowClear
              showSearch
              optionFilterProp="label"
              value={planRepositoryIds}
              onChange={setPlanRepositoryIds}
              options={repositories.map((repo) => ({ value: repo.id, label: `${repo.name} · ${repo.branch}` }))}
              placeholder={t('rd.repositories', '选择参与仓库')}
            />
            <Input
              value={title}
              onChange={(event) => setTitle(event.target.value)}
              placeholder={t('rd.planTitlePlaceholder', '标题，可选')}
            />
            <Input.TextArea
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              placeholder={t('rd.planPromptPlaceholder', '描述完整需求，AOS 会生成 Spec -> Design -> Tasks')}
              autoSize={{ minRows: 5, maxRows: 10 }}
            />
            <Button
              type="primary"
              icon={<FileTextOutlined />}
              disabled={!prompt.trim()}
              loading={createSpecMutation.isPending}
              onClick={() => createSpecMutation.mutate()}
            >
              {t('rd.generateSpec', '生成 Spec')}
            </Button>
          </div>
          <div className="rd-plan-spec-list">
            <Text strong>{t('rd.planList', '计划列表')}</Text>
            {specsQuery.isLoading ? <Spin /> : specs.length === 0 ? (
              <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.noPlans', '暂无计划')} />
            ) : specs.map((spec) => (
              <button
                key={spec.id}
                type="button"
                className={`rd-plan-list-item${spec.id === selectedSpecId ? ' rd-plan-list-item-active' : ''}`}
                onClick={() => {
                  setSelectedSpecId(spec.id);
                  setDraftSpec(null);
                  if (spec.repositoryId) onSelectRepo(spec.repositoryId);
                }}
              >
                <span>{spec.title}</span>
                <small>{spec.currentStage || 'spec'} · {spec.status}</small>
              </button>
            ))}
          </div>
        </Space>
      </aside>

      <main className="rd-plan-main">
        {!selectedSpec ? (
          <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t('rd.selectOrCreatePlan', '选择或创建一个 Spec')} />
        ) : (
          <Space direction="vertical" size={12} style={{ width: '100%', minWidth: 0 }}>
            <div className="rd-plan-header">
              <Space direction="vertical" size={4} style={{ minWidth: 0 }}>
                <Space wrap>
                  <Title level={4}>{selectedSpec.title}</Title>
                  <Tag color="cyan">{t('rd.specMode', 'Spec')}</Tag>
                  <Tag>{selectedSpec.status}</Tag>
                  <Tag color="blue">{selectedSpec.currentStage || 'spec'}</Tag>
                </Space>
                <Text type="secondary" ellipsis={{ tooltip: selectedSpec.prompt }}>{selectedSpec.prompt}</Text>
              </Space>
              <Space>
                <Button
                  size="small"
                  icon={<ReloadOutlined />}
                  onClick={() => invalidateSpec(selectedSpec.id)}
                >
                  {t('common.refresh', '刷新')}
                </Button>
                <Popconfirm
                  title={t('rd.planDeleteConfirm', '确认删除该研发计划？')}
                  onConfirm={() => deleteSpecMutation.mutate(selectedSpec.id)}
                  okText={t('common.delete')}
                  cancelText={t('common.cancel')}
                  disabled={['queued', 'running'].includes(selectedSpec.status)}
                >
                  <Button
                    size="small"
                    danger
                    icon={<DeleteOutlined />}
                    disabled={['queued', 'running'].includes(selectedSpec.status)}
                    loading={deleteSpecMutation.isPending}
                  >
                    {t('common.delete')}
                  </Button>
                </Popconfirm>
              </Space>
            </div>
            <PlanStageStepper currentStage={selectedSpec.currentStage} />
            {selectedSpec.lastError ? <Alert type="error" showIcon message={selectedSpec.lastError} /> : null}
            <Tabs
              className="rd-plan-tabs"
              items={[
                {
                  key: 'spec',
                  label: t('rd.planStages.spec', 'Spec'),
                  children: (
                    <Space direction="vertical" size={12} style={{ width: '100%' }}>
                      <SpecEditor
                        title={t('rd.requirementsDoc', '需求规格')}
                        value={selectedSpec.requirementsMd}
                        placeholder={t('rd.requirementsDocEmpty', '生成后会显示需求、约束和验收标准')}
                        onChange={(requirementsMd) => setDraftSpec({ ...selectedSpec, requirementsMd })}
                      />
                      <Space wrap>
                        <Button loading={stageMutation.isPending} onClick={() => stageMutation.mutate('generateSpec')}>
                          {t('rd.regenerateSpec', '重新生成核心设计')}
                        </Button>
                        <Button loading={updateSpecMutation.isPending} onClick={() => updateSpecMutation.mutate({ requirementsMd: selectedSpec.requirementsMd })}>
                          {t('common.save', '保存')}
                        </Button>
                        <Button
                          type="primary"
                          icon={<CheckCircleOutlined />}
                          disabled={!selectedSpec.requirementsMd?.trim()}
                          loading={stageMutation.isPending}
                          onClick={() => stageMutation.mutate('approveSpec')}
                        >
                          {t('rd.approveSpec', '确认核心设计')}
                        </Button>
                      </Space>
                    </Space>
                  ),
                },
                {
                  key: 'design',
                  label: t('rd.planStages.design', 'Design'),
                  children: (
                    <Space direction="vertical" size={12} style={{ width: '100%' }}>
                      <SpecEditor
                        title={t('rd.designDoc', '设计文档')}
                        value={selectedSpec.designMd}
                        placeholder={t('rd.designDocEmpty', '确认核心设计后生成架构设计、影响范围、风险和接口变更')}
                        onChange={(designMd) => setDraftSpec({ ...selectedSpec, designMd })}
                      />
                      <Space wrap>
                        <Button disabled={!canGenerateDesign(selectedSpec)} loading={stageMutation.isPending} onClick={() => stageMutation.mutate('generateDesign')}>
                          {t('rd.generateDesign', '生成 Design')}
                        </Button>
                        <Button loading={updateSpecMutation.isPending} onClick={() => updateSpecMutation.mutate({ designMd: selectedSpec.designMd })}>
                          {t('common.save', '保存')}
                        </Button>
                        <Button
                          type="primary"
                          icon={<CheckCircleOutlined />}
                          disabled={!selectedSpec.designMd?.trim() || !canGenerateDesign(selectedSpec)}
                          loading={stageMutation.isPending}
                          onClick={() => stageMutation.mutate('approveDesign')}
                        >
                          {t('rd.approveDesign', '确认 Design')}
                        </Button>
                      </Space>
                    </Space>
                  ),
                },
                {
                  key: 'tasks',
                  label: t('rd.planStages.tasks', 'Tasks'),
                  children: (
                    <Space direction="vertical" size={12} style={{ width: '100%' }}>
                      <SpecEditor
                        title={t('rd.tasksDoc', '任务拆解')}
                        value={selectedSpec.tasksMd}
                        placeholder={t('rd.tasksDocEmpty', '确认代码研发方案后生成有序 Task')}
                        onChange={(tasksMd) => setDraftSpec({ ...selectedSpec, tasksMd })}
                      />
                      <Space wrap>
                        <Button disabled={!canGenerateTasks(selectedSpec)} loading={stageMutation.isPending} onClick={() => stageMutation.mutate('generateTasks')}>
                          {t('rd.generateTasks', '生成 Tasks')}
                        </Button>
                        <Button loading={updateSpecMutation.isPending} onClick={() => updateSpecMutation.mutate({ tasksMd: selectedSpec.tasksMd, taskItems })}>
                          {t('common.save', '保存')}
                        </Button>
                        <Button
                          type="primary"
                          icon={<CheckCircleOutlined />}
                          disabled={taskItems.length === 0 || !canGenerateTasks(selectedSpec)}
                          loading={stageMutation.isPending}
                          onClick={() => stageMutation.mutate('approveTasks')}
                        >
                          {t('rd.approveTasks', '确认 Tasks')}
                        </Button>
                      </Space>
                    </Space>
                  ),
                },
                {
                  key: 'implementation',
                  label: t('rd.planStages.implementation', 'Implement'),
                  children: (
                    <Space direction="vertical" size={12} style={{ width: '100%' }}>
                      <Alert
                        type="info"
                        showIcon
                        message={t('rd.planImplementationHint', 'Implementation 会为每个任务项创建真实 RD task')}
                        description={t('rd.planImplementationDesc', '每个 RD task 仍走候选工作区、Runtime、Diff-first 审批和 WatchDog 观测，不会静默修改主仓库。')}
                      />
                      <Space wrap>
                        <Button
                          type="primary"
                          icon={<PlayCircleOutlined />}
                          disabled={!canImplement(selectedSpec)}
                          loading={implementAllMutation.isPending}
                          onClick={() => implementAllMutation.mutate()}
                        >
                          {t('rd.implementAllTasks', '执行全部')}
                        </Button>
                        <Button
                          disabled={!selectedSpec.approvedTasksAt}
                          loading={stageMutation.isPending}
                          onClick={() => stageMutation.mutate('finalReport')}
                        >
                          {t('rd.generateFinalReport', '生成 Final Report')}
                        </Button>
                      </Space>
                      <TaskItemBoard
                        items={taskItems}
                        loadingTaskId={implementingTaskId}
                        canImplement={canImplement(selectedSpec)}
                        onImplement={(item) => implementTaskMutation.mutate(item)}
                        onOpenTask={onOpenTask}
                      />
                    </Space>
                  ),
                },
                {
                  key: 'events',
                  label: t('rd.planEvents', '事件'),
                  children: (
                    <Space direction="vertical" size={8} style={{ width: '100%' }}>
                      {(specEventsQuery.data ?? []).map((event) => (
                        <div key={event.id} className="rd-plan-event">
                          <Space wrap>
                            <Tag>{event.eventType}</Tag>
                            {event.stage ? <Tag color="blue">{event.stage}</Tag> : null}
                            {event.status ? <Tag>{event.status}</Tag> : null}
                          </Space>
                          <Text>{event.message}</Text>
                          <Text type="secondary">{event.createdAt}</Text>
                        </div>
                      ))}
                    </Space>
                  ),
                },
              ]}
            />
          </Space>
        )}
      </main>
    </div>
  );
}
