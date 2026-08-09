import { client, fastClient } from './client';

export interface DemoRunSummary {
  runId: string;
  agentTaskId: string;
  scenarioId: string;
  status: string;
  entryPath: string;
  capabilityKey: string;
  message: string;
  createdAt: string;
  completedAt?: string | null;
}

export interface DemoScenario {
  id: string;
  title: string;
  titleZh: string;
  summary: string;
  summaryZh: string;
  capabilityKey: string;
  entryPath: string;
  cta: string;
  ctaZh: string;
  prompt: string;
  promptZh: string;
  assets: string[];
  setupSteps: string[];
  expected: string[];
  status: 'ready' | 'degraded' | string;
  featureReady: boolean;
  missingFeature?: string | null;
  lastRun?: DemoRunSummary | null;
}

export const demoApi = {
  listScenarios: () =>
    fastClient.get<{ items: DemoScenario[] }>('/demo/scenarios').then((r) => r.data),

  runScenario: (id: string) =>
    client.post<DemoRunSummary>(`/demo/scenarios/${encodeURIComponent(id)}/run`, {}).then((r) => r.data),

  scenarioStatus: (id: string) =>
    fastClient.get<DemoScenario>(`/demo/scenarios/${encodeURIComponent(id)}/status`).then((r) => r.data),
};
