import { client } from './client';

export type ConfigEnvValueType = 'bool' | 'integer' | 'float' | 'secret' | 'string';

export interface ConfigEnvEntry {
  key: string;
  label: string;
  description: string;
  valueType: ConfigEnvValueType;
  value: string;
  defaultValue: string;
  source: string;
}

export interface ConfigPmBudgetProfile {
  profileKey: string;
  enabled: boolean;
  isDefault: boolean;
  priority: number;
  pipelineTimeoutSecs: number;
  maxAttempts: number;
  retrieveMaxToolCalls: number;
  maxCallsPerSource: number;
  sourceSlotSearchSecs: number;
  sourceSlotBrowserSecs: number;
  sourceSlotApiFetchSecs: number;
  preflightModelTimeoutSecs: number;
  preflightProbeTimeoutSecs: number;
  preflightOverallTimeoutSecs: number;
  retryStepBudgetSecs: number;
  retryTotalBudgetSecs: number;
  source: string;
}

export interface ConfigManagementTab {
  env: ConfigEnvEntry[];
  pmBudgetProfile?: ConfigPmBudgetProfile | null;
}

export interface ConfigManagementOverview {
  operations: ConfigManagementTab;
  analytics: ConfigManagementTab;
  engineering: ConfigManagementTab;
}

export const configApi = {
  getOverview: () =>
    client.get('/config').then((r) => r.data),
  getManagementOverview: () =>
    client.get<ConfigManagementOverview>('/config/management').then((r) => r.data),
  updateManagementEnv: (data: { key: string; value?: string | null; clear?: boolean }) =>
    client.patch<ConfigEnvEntry>('/config/management/env', data).then((r) => r.data),
  updateManagementPmBudgetProfile: (data: Partial<{
    profileKey: string;
    enabled: boolean;
    isDefault: boolean;
    priority: number;
    pipelineTimeoutSecs: number;
    maxAttempts: number;
    retrieveMaxToolCalls: number;
    maxCallsPerSource: number;
    sourceSlotSearchSecs: number;
    sourceSlotBrowserSecs: number;
    sourceSlotApiFetchSecs: number;
    preflightModelTimeoutSecs: number;
    preflightProbeTimeoutSecs: number;
    preflightOverallTimeoutSecs: number;
    retryStepBudgetSecs: number;
    retryTotalBudgetSecs: number;
  }>) =>
    client.patch<ConfigPmBudgetProfile>('/config/management/pm-budget-profile', data).then((r) => r.data),
};
