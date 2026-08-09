import { client } from './client';
import type {
  McpPromptInfo,
  McpResourceInfo,
  McpServerInfo,
  McpStats,
  McpToolInfo,
} from '@/types';

export const mcpApi = {
  list: (params?: { page?: number; per_page?: number }) =>
    client.get<{ servers: McpServerInfo[]; total: number }>('/mcp', { params }).then((r) => r.data),

  stats: () => client.get<McpStats>('/mcp/stats').then((r) => r.data),

  add: (data: {
    name: string;
    transport: string;
    command?: string;
    args?: string[];
    env?: Record<string, string>;
    url?: string;
    auth_type?: string;
    auth_token?: string;
    extra_headers?: Record<string, string>;
    timeout_ms?: number;
  }) =>
    client.post<McpServerInfo>('/mcp', data).then((r) => r.data),

  remove: (name: string) =>
    client.delete(`/mcp/${encodeURIComponent(name)}`).then((r) => r.data),

  get: (name: string) =>
    client.get<McpServerInfo>(`/mcp/${encodeURIComponent(name)}`).then((r) => r.data),

  update: (name: string, data: {
    name?: string;
    transport?: string;
    command?: string;
    args?: string[];
    env?: Record<string, string>;
    url?: string;
    enabled?: boolean;
    auth_type?: string;
    auth_token?: string;
    extra_headers?: Record<string, string>;
    timeout_ms?: number;
  }) =>
    client.patch<McpServerInfo>(`/mcp/${encodeURIComponent(name)}`, data).then((r) => r.data),

  toggle: (name: string, enabled: boolean) =>
    client.patch<McpServerInfo>(`/mcp/${encodeURIComponent(name)}/toggle`, { enabled }).then((r) => r.data),

  retry: (name: string) =>
    client.post<McpServerInfo>(`/mcp/${encodeURIComponent(name)}/retry`, {}).then((r) => r.data),

  test: (name: string) =>
    client.post<{ success: boolean; tools_count: number; error?: string }>(`/mcp/${encodeURIComponent(name)}/test`, {}).then((r) => r.data),

  /** Low-level connectivity test — returns latency in milliseconds. */
  testConnection: (name: string) =>
    client.post<{ success: boolean; latency_ms: number; error?: string }>(`/mcp/${encodeURIComponent(name)}/test-connection`, {}).then((r) => r.data),

  /** Live-discover tools from an HTTP/SSE MCP server. */
  listTools: (name: string) =>
    client.get<{ tools: McpToolInfo[]; count: number }>(`/mcp/${encodeURIComponent(name)}/tools`).then((r) => r.data),

  /** Live-discover resources from an HTTP/SSE MCP server. */
  listResources: (name: string) =>
    client.get<{ resources: McpResourceInfo[]; count: number }>(`/mcp/${encodeURIComponent(name)}/resources`).then((r) => r.data),

  /** Live-discover prompts from an HTTP/SSE MCP server. */
  listPrompts: (name: string) =>
    client.get<{ prompts: McpPromptInfo[]; count: number }>(`/mcp/${encodeURIComponent(name)}/prompts`).then((r) => r.data),
};
