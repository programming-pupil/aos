import { client } from './client';

export interface WorkspaceFileItem {
  name: string;
  path: string;
  kind: 'directory' | 'file';
  sizeBytes: number;
  updatedAt?: string | null;
  editable: boolean;
}

export interface WorkspaceFilePage {
  path: string;
  absolutePath: string;
  items: WorkspaceFileItem[];
  nextCursor?: string | null;
  hasMore: boolean;
}

export interface WorkspaceUploadItem {
  fileId: string;
  filename: string;
  mediaType: string;
  sizeBytes: number;
  url: string;
  status: string;
  errorMessage?: string | null;
  sessionId?: string | null;
  updatedAt: string;
}

export interface WorkspaceUploadPage {
  items: WorkspaceUploadItem[];
  nextCursor?: string | null;
  hasMore: boolean;
}

export interface WorkspaceCommandResult {
  status: 'succeeded' | 'failed' | 'timed_out' | 'cancelled';
  exitCode?: number | null;
  timedOut: boolean;
  cancelled: boolean;
  durationMs: number;
  stdout: string;
  stderr: string;
  cwd: string;
}

export interface WorkspaceSchedule {
  id: string;
  sessionId?: string | null;
  name: string;
  scriptPath?: string | null;
  command: string;
  cwd: string;
  cronExpression: string;
  timezone: string;
  timeoutSeconds: number;
  enabled: boolean;
  status: string;
  nextRunAt?: string | null;
  lastStartedAt?: string | null;
  lastFinishedAt?: string | null;
  lastExitCode?: number | null;
  lastStdout?: string | null;
  lastStderr?: string | null;
  runCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface WorkspaceScheduleInput {
  name: string;
  command: string;
  cwd: string;
  cronExpression: string;
  timezone: string;
  timeoutSeconds: number;
  scriptPath?: string | null;
}

export const personalWorkspaceApi = {
  listFiles: (params: { path: string; cursor?: string | null; limit?: number }) =>
    client
      .get<WorkspaceFilePage>('/workspace/items', {
        params: { path: params.path, cursor: params.cursor || undefined, limit: params.limit },
      })
      .then((response) => response.data),

  readFile: (path: string) =>
    client
      .get<{ path: string; content: string; sizeBytes: number; updatedAt?: string | null }>(
        '/workspace/items/content',
        { params: { path } },
      )
      .then((response) => response.data),

  saveFile: (data: { path: string; content: string; overwrite?: boolean }) =>
    client.post<{ path: string; saved: boolean; sizeBytes: number }>('/workspace/items', data).then((response) => response.data),

  createDirectory: (path: string) =>
    client.post<{ path: string; created: boolean }>('/workspace/directories', { path }).then((response) => response.data),

  renameItem: (path: string, newName: string) =>
    client.post<{ path: string; renamed: boolean }>('/workspace/rename', { path, newName }).then((response) => response.data),

  deleteItem: (path: string, recursive = false) =>
    client.delete<{ deleted: boolean }>('/workspace/items', { params: { path, recursive } }).then((response) => response.data),

  uploadFile: (path: string, file: File) => {
    const body = new FormData();
    body.append('file', file);
    return client
      .post<{ path: string; filename: string; sizeBytes: number; uploaded: boolean }>(
        '/workspace/upload',
        body,
        { params: { path } },
      )
      .then((response) => response.data);
  },

  downloadFile: (path: string) =>
    client.get<Blob>('/workspace/items/download', { params: { path }, responseType: 'blob' }).then((response) => response.data),

  listUploads: (params?: { cursor?: string | null; limit?: number }) =>
    client
      .get<WorkspaceUploadPage>('/workspace/uploads', {
        params: { cursor: params?.cursor || undefined, limit: params?.limit },
      })
      .then((response) => response.data),

  deleteUpload: (fileId: string) =>
    client.delete<{ deleted: boolean; fileId: string }>(`/workspace/uploads/${encodeURIComponent(fileId)}`).then((response) => response.data),

  downloadUpload: (url: string) => {
    const path = url.startsWith('/api/v1/') ? url.slice('/api/v1'.length) : url;
    return client.get<Blob>(path, { responseType: 'blob' }).then((response) => response.data);
  },

  executeCommand: (
    data: { command: string; cwd: string; timeoutSeconds: number },
    signal?: AbortSignal,
  ) =>
    client.post<WorkspaceCommandResult>('/workspace/commands', data, { signal }).then((response) => response.data),

  listSchedules: () =>
    client.get<WorkspaceSchedule[]>('/workspace/schedules').then((response) => response.data),

  createSchedule: (data: WorkspaceScheduleInput) =>
    client.post<WorkspaceSchedule>('/workspace/schedules', data).then((response) => response.data),

  updateSchedule: (id: string, data: Partial<WorkspaceScheduleInput> & { enabled?: boolean }) =>
    client.patch<WorkspaceSchedule>(`/workspace/schedules/${encodeURIComponent(id)}`, data).then((response) => response.data),

  cancelSchedule: (id: string) =>
    client.delete<WorkspaceSchedule>(`/workspace/schedules/${encodeURIComponent(id)}`).then((response) => response.data),
};
