import type { McpServerInfo } from '@/types';

export const MCP_STDIO_REDACTED_ENV_VALUE = '********';

export type McpStdioConfigErrorCode =
  | 'invalidJson'
  | 'rootObjectRequired'
  | 'serversObjectRequired'
  | 'singleServerRequired'
  | 'invalidServerName'
  | 'serverObjectRequired'
  | 'stdioOnly'
  | 'unsupportedFields'
  | 'commandRequired'
  | 'commandNoArgs'
  | 'argsMustBeArray'
  | 'argsMustBeStrings'
  | 'envMustBeObject'
  | 'envValuesMustBeStrings';

export class McpStdioConfigError extends Error {
  constructor(
    public readonly code: McpStdioConfigErrorCode,
    public readonly detail?: string,
  ) {
    super(code);
  }
}

export interface ParsedMcpStdioConfig {
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function parseMcpStdioConfig(raw: string): ParsedMcpStdioConfig {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new McpStdioConfigError('invalidJson');
  }
  if (!isRecord(parsed)) {
    throw new McpStdioConfigError('rootObjectRequired');
  }
  const servers = parsed.mcpServers;
  if (!isRecord(servers)) {
    throw new McpStdioConfigError('serversObjectRequired');
  }
  const entries = Object.entries(servers);
  if (entries.length !== 1) {
    throw new McpStdioConfigError('singleServerRequired');
  }
  const [name, config] = entries[0];
  if (!/^[a-zA-Z0-9_-]+$/.test(name)) {
    throw new McpStdioConfigError('invalidServerName');
  }
  if (!isRecord(config)) {
    throw new McpStdioConfigError('serverObjectRequired');
  }
  if (config.type !== undefined && config.type !== 'stdio') {
    throw new McpStdioConfigError('stdioOnly');
  }
  const allowedFields = new Set(['type', 'command', 'args', 'env']);
  const unsupportedFields = Object.keys(config).filter((key) => !allowedFields.has(key));
  if (unsupportedFields.length > 0) {
    throw new McpStdioConfigError('unsupportedFields', unsupportedFields.join(', '));
  }
  if (typeof config.command !== 'string' || !config.command.trim()) {
    throw new McpStdioConfigError('commandRequired');
  }
  const command = config.command.trim();
  if (/\s/.test(command)) {
    throw new McpStdioConfigError('commandNoArgs');
  }
  if (config.args !== undefined && !Array.isArray(config.args)) {
    throw new McpStdioConfigError('argsMustBeArray');
  }
  if (Array.isArray(config.args) && config.args.some((arg) => typeof arg !== 'string')) {
    throw new McpStdioConfigError('argsMustBeStrings');
  }
  if (config.env !== undefined && !isRecord(config.env)) {
    throw new McpStdioConfigError('envMustBeObject');
  }
  if (isRecord(config.env) && Object.values(config.env).some((value) => typeof value !== 'string')) {
    throw new McpStdioConfigError('envValuesMustBeStrings');
  }

  return {
    name,
    command,
    args: (config.args as string[] | undefined) ?? [],
    env: (config.env as Record<string, string> | undefined) ?? {},
  };
}

export function formatMcpStdioConfig(server?: Pick<McpServerInfo, 'name' | 'command' | 'args' | 'env'>): string {
  const name = server?.name || 'weather';
  const config: Record<string, unknown> = {
    command: server?.command || 'uv',
    args: server?.args?.length
      ? server.args
      : ['--directory', 'weather-my-mcp', 'run', 'weather.py'],
  };
  if (server?.env && Object.keys(server.env).length > 0) {
    config.env = server.env;
  }
  return JSON.stringify({ mcpServers: { [name]: config } }, null, 2);
}
