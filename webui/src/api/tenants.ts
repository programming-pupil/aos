import { client } from './client';

export const tenantsApi = {
  list: (params?: { page?: number; per_page?: number }) =>
    client.get<import('@/types').TenantListResponse>('/tenants', { params }).then((r) => r.data),

  get: (id: string) =>
    client.get<import('@/types').TenantInfo>(`/tenants/${encodeURIComponent(id)}`).then((r) => r.data),

  create: (data: { name: string; slug: string; plan: string; max_users?: number; max_tokens_monthly?: number }) =>
    client.post<import('@/types').TenantInfo>('/tenants', data).then((r) => r.data),

  update: (id: string, data: Partial<{ name: string; slug: string; plan: string; max_users?: number; max_tokens_monthly?: number }>) =>
    client.patch<import('@/types').TenantInfo>(`/tenants/${encodeURIComponent(id)}`, data).then((r) => r.data),

  delete: (id: string) =>
    client.delete(`/tenants/${encodeURIComponent(id)}`).then((r) => r.data),

  getUsage: (id: string) =>
    client.get<{
      tenant_id: string;
      usage_this_month: number;
      max_tokens_monthly?: number | null;
      user_count: number;
      max_users?: number | null;
      usage_percent: number;
      over_limit: boolean;
    }>(`/tenants/${encodeURIComponent(id)}/usage`).then((r) => r.data),
};
