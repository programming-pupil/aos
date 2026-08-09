import { client } from './client';
import type { SkillInfo, SkillSecurityScan, UploadZipResult } from '@/types';

export const skillsApi = {
  githubTokenStatus: () =>
    client
      .get<{ configured: boolean }>('/skills/market/github-token-status')
      .then((r) => r.data),

  list: (params?: { page?: number; per_page?: number }) =>
    client.get<{ skills: SkillInfo[]; total: number }>('/skills', { params }).then((r) => r.data),

  get: (name: string) =>
    client.get<SkillInfo>(`/skills/${encodeURIComponent(name)}`).then((r) => r.data),

  readme: (name: string) =>
    client.get<string>(`/skills/${encodeURIComponent(name)}/readme`).then((r) => r.data),

  /** List commands in the skill's commands/ directory. */
  commands: (name: string) =>
    client.get<import('@/types').SkillCommand[]>(`/skills/${encodeURIComponent(name)}/commands`).then((r) => r.data),

  /** Save (update) the SKILL.md content for a skill. Triggers hot-reload. */
  saveReadme: ({ name, content }: { name: string; content: string }) =>
    client.patch<SkillInfo>(`/skills/${encodeURIComponent(name)}`, { skill_md_content: content }).then((r) => r.data),

  /** Upload a new skill. Sends the SKILL.md content directly as JSON. */
  upload: (data: {
    name: string;
    description?: string;
    tags?: string[];
    skill_md_content: string;
  }) =>
    client.post<SkillInfo>('/skills', data).then((r) => r.data),

  /** Update skill metadata (description, tags, enabled, skill_md_content). */
  update: (name: string, data: {
    description?: string;
    tags?: string[];
    enabled?: boolean;
    skill_md_content?: string;
  }) =>
    client.patch<SkillInfo>(`/skills/${encodeURIComponent(name)}`, data).then((r) => r.data),

  /** Delete a skill (soft delete by default). */
  delete: (name: string, permanentlyDelete?: boolean) => {
    const params = permanentlyDelete ? { permanently_delete: true } : {};
    return client.delete(`/skills/${encodeURIComponent(name)}`, { params }).then((r) => r.data);
  },

  /** Upload a skill as a .zip archive. Returns the created skill and any security warnings. */
  /** Preview a zip: extract name/description/tags from SKILL.md without persisting. */
  previewZip: (file: File) => {
    const form = new FormData();
    form.append('file', file);
    return client.post<{ name: string; description?: string; tags: string[]; warnings: string[]; securityScan: SkillSecurityScan }>('/skills/zip/preview', form, {
      headers: { 'Content-Type': 'multipart/form-data' },
      timeout: 35_000,
    }).then((r) => r.data);
  },

  /** Upload a skill from a zip file. */
  uploadZip: (file: File, name?: string, description?: string, tags?: string[], riskConfirmed = false) => {
    const form = new FormData();
    form.append('file', file);
    if (name) form.append('name', name);
    if (description) form.append('description', description);
    if (tags?.length) form.append('tags', JSON.stringify(tags));
    form.append('riskConfirmed', String(riskConfirmed));
    return client.post<UploadZipResult>('/skills/zip', form, {
      headers: { 'Content-Type': 'multipart/form-data' },
    }).then((r) => r.data);
  },

  /** Enable or disable a skill. */
  toggle: (name: string, enabled: boolean) =>
    client.post<SkillInfo>(`/skills/${encodeURIComponent(name)}/toggle`, { enabled }).then((r) => r.data),

  /** Create a skill from marketplace data (placeholder). */
  create: (data: {
    name: string;
    description?: string;
    tags?: string[];
    skill_md_content: string;
  }) =>
    client.post<SkillInfo>('/skills', data).then((r) => r.data),

  // ---- Skills Market / Repository management (additive; does not replace existing flows) ----
  listMarketRepositories: (params?: { page?: number; per_page?: number }) =>
    client.get<{ items: Array<{
      id: string;
      tenantId?: string | null;
      repoFullName: string;
      repoUrl: string;
      branch: string;
      enabled: boolean;
      discoveredCount: number;
      lastScanAt?: string | null;
      lastScanStatus: string;
      lastScanError?: string | null;
      createdBy?: string | null;
      createdAt?: string | null;
      updatedAt?: string | null;
      builtIn: boolean;
    }>; total: number; page: number; perPage: number; hasMore: boolean }>('/skills/market/repositories', { params }).then((r) => r.data),

  addMarketRepository: (data: { repoUrl: string; branch?: string }) =>
    client.post<{
      id: string;
      tenantId?: string | null;
      repoFullName: string;
      repoUrl: string;
      branch: string;
      enabled: boolean;
      discoveredCount: number;
      lastScanAt?: string | null;
      lastScanStatus: string;
      lastScanError?: string | null;
      createdBy?: string | null;
      createdAt?: string | null;
      updatedAt?: string | null;
      builtIn: boolean;
    }>('/skills/market/repositories', data).then((r) => r.data),

  deleteMarketRepository: (id: string) =>
    client.delete(`/skills/market/repositories/${encodeURIComponent(id)}`).then((r) => r.data),

  scanMarketRepository: (id: string) =>
    client.post<{
      id: string;
      tenantId?: string | null;
      repoFullName: string;
      repoUrl: string;
      branch: string;
      enabled: boolean;
      discoveredCount: number;
      lastScanAt?: string | null;
      lastScanStatus: string;
      lastScanError?: string | null;
      createdBy?: string | null;
      createdAt?: string | null;
      updatedAt?: string | null;
      builtIn: boolean;
    }>(`/skills/market/repositories/${encodeURIComponent(id)}/scan`).then((r) => r.data),

  searchMarketSkills: (params?: { q?: string; page?: number; per_page?: number; limit?: number }) =>
    client.get<{ items: Array<{
      id: string;
      repoFullName: string;
      repoUrl: string;
      branch: string;
      skillName: string;
      skillPath: string;
      readmeUrl?: string | null;
      htmlUrl?: string | null;
      sourceType: string;
    }>; total: number; page: number; perPage: number; hasMore: boolean }>('/skills/market/search', { params }).then((r) => r.data),

  installMarketSkill: (data: {
    repoFullName: string;
    repoUrl?: string;
    branch: string;
    skillPath: string;
    installName?: string;
  }) =>
    client.post<{
      skill: SkillInfo;
      installedFrom: {
        id: string;
        repoFullName: string;
        repoUrl: string;
        branch: string;
        skillName: string;
        skillPath: string;
        readmeUrl?: string | null;
        htmlUrl?: string | null;
        sourceType: string;
      };
    }>('/skills/market/install', data).then((r) => r.data),
};
