import { Steps } from 'antd';
import { useTranslation } from 'react-i18next';
import type { RdSpecStage } from '@/types';

const STAGES: Array<{ key: RdSpecStage; labelKey: string; fallback: string }> = [
  { key: 'spec', labelKey: 'rd.planStages.spec', fallback: 'Spec' },
  { key: 'design', labelKey: 'rd.planStages.design', fallback: 'Design' },
  { key: 'tasks', labelKey: 'rd.planStages.tasks', fallback: 'Tasks' },
  { key: 'implementation', labelKey: 'rd.planStages.implementation', fallback: 'Implement' },
  { key: 'verify', labelKey: 'rd.planStages.verify', fallback: 'Verify' },
  { key: 'final', labelKey: 'rd.planStages.final', fallback: 'Final' },
];

const STAGE_ORDER = new Map<RdSpecStage | string, number>(STAGES.map((stage, index) => [stage.key, index]));

export function PlanStageStepper({ currentStage }: { currentStage?: RdSpecStage | string | null }) {
  const { t } = useTranslation();
  const current = STAGE_ORDER.get(currentStage || 'spec') ?? 0;
  return (
    <Steps
      size="small"
      current={current}
      items={STAGES.map((stage, index) => ({
        title: t(stage.labelKey, stage.fallback),
        status: index < current ? 'finish' : index === current ? 'process' : 'wait',
      }))}
    />
  );
}
