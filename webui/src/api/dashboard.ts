import { client } from './client';
import type {
  DashboardConfigOverviewStats,
  DashboardOverview,
  DailyTokenStats,
  ModelUsageStats,
  ModuleTokenUsageStats,
} from '@/types';

export const dashboardApi = {
  getOverview: (params?: { start_date?: string; end_date?: string }) =>
    client.get<DashboardOverview>('/dashboard/overview', { params }).then((r) => r.data),

  getConfigOverviewStats: () =>
    client.get<DashboardConfigOverviewStats>('/dashboard/config-overview-stats').then((r) => r.data),

  getDailyTrend: (params?: { start_date?: string; end_date?: string }) =>
    client.get<DailyTokenStats[]>('/dashboard/daily-trend', { params }).then((r) => r.data),

  getModelUsage: (params?: { start_date?: string; end_date?: string }) =>
    client.get<ModelUsageStats[]>('/dashboard/model-usage', { params }).then((r) => r.data),

  getModuleUsage: (params?: { start_date?: string; end_date?: string }) =>
    client.get<ModuleTokenUsageStats[]>('/dashboard/module-usage', { params }).then((r) => r.data),

  listAlerts: () =>
    client.get<import('@/types').UsageAlertListResponse>('/dashboard/alerts').then((r) => r.data),

  createAlert: (data: {
    name: string;
    alert_type: 'daily_budget' | 'monthly_budget' | 'per_key_limit';
    threshold_tokens: number;
    threshold_usd?: number;
  }) =>
    client.post<import('@/types').UsageAlertInfo>('/dashboard/alerts', data).then((r) => r.data),

  updateAlert: (id: string, data: {
    name?: string;
    alert_type?: string;
    threshold_tokens?: number;
    threshold_usd?: number;
    enabled?: boolean;
  }) =>
    client.patch<import('@/types').UsageAlertInfo>(`/dashboard/alerts/${encodeURIComponent(id)}`, data).then((r) => r.data),

  deleteAlert: (id: string) =>
    client.delete(`/dashboard/alerts/${encodeURIComponent(id)}`).then((r) => r.data),
};
