import { client } from './client';

export const hooksApi = {
  list: (params?: { page?: number; per_page?: number }) =>
    client.get<{ hooks: import('@/types').HookInfo[]; total: number }>('/hooks', { params }).then((r) => r.data),

  create: (data: {
    event_type: import('@/types').HookEventType;
    name: string;
    language?: string;
    code?: string;
    command?: string;
    enabled?: boolean;
    priority?: number;
    timeout_seconds?: number;
    fail_fast?: boolean;
    description?: string;
    scenarios?: string[];
  }) =>
    client.post<import('@/types').HookInfo>('/hooks', data).then((r) => r.data),

  get: (id: string) =>
    client.get<import('@/types').HookInfo>(`/hooks/${id}`).then((r) => r.data),

  update: (id: string, data: {
    event_type?: import('@/types').HookEventType;
    name?: string;
    language?: string;
    code?: string;
    command?: string;
    enabled?: boolean;
    priority?: number;
    timeout_seconds?: number;
    fail_fast?: boolean;
    description?: string;
    scenarios?: string[];
  }) =>
    client.patch<import('@/types').HookInfo>(`/hooks/${id}`, data).then((r) => r.data),

  delete: (id: string) =>
    client.delete(`/hooks/${id}`).then((r) => r.data),

  logs: (id: string, params?: { page?: number; per_page?: number }) =>
    client.get<{ logs: import('@/types').HookLogEntry[]; total: number }>(`/hooks/${id}/logs`, { params }).then((r) => r.data),

  validate: (data: { code: string; language: string }) =>
    client.post<import('@/types').HookValidationResponse>('/hooks/validate', data).then((r) => r.data),

  dryRun: (id: string, data: {
    event_type?: import('@/types').HookEventType;
    scenario?: string;
    tool_name: string;
    tool_input: unknown;
    tool_output?: unknown;
    is_error?: boolean;
  }) =>
    client.post<{
      stdout?: string | null;
      stderr?: string | null;
      exit_code: number;
      duration_ms: number;
      status?: string;
      diagnostics?: string[];
      error?: string | null;
    }>(`/hooks/${id}/dry-run`, data).then((r) => r.data),
};
