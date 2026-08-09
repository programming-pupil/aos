import { client } from './client';
import type { CommandInfo, GitlabProject } from '@/types';

export const commandsApi = {
  list: () =>
    client.get<{ builtin: CommandInfo[]; skills: CommandInfo[] }>('/agent/commands').then((r) => r.data),
};

export const projectsApi = {
  list: () =>
    client.get<{ projects: GitlabProject[]; total: number }>('/projects').then((r) => r.data),

  add: (data: { name: string; url: string; branch?: string; gitlab_token?: string; description?: string }) =>
    client.post<GitlabProject>('/projects', data).then((r) => r.data),

  get: (id: string) =>
    client.get<GitlabProject>(`/projects/${encodeURIComponent(id)}`).then((r) => r.data),

  delete: (id: string) =>
    client.delete(`/projects/${encodeURIComponent(id)}`).then((r) => r.data),

  sync: (id: string) =>
    client.post<{ synced: boolean; clone_path: string }>(`/projects/${encodeURIComponent(id)}/sync`, {}).then((r) => r.data),
};
