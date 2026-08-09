import { describe, expect, it } from 'vitest';
import {
  McpStdioConfigError,
  formatMcpStdioConfig,
  parseMcpStdioConfig,
} from './mcpStdioConfig';

describe('stdio MCP JSON config', () => {
  it('parses the standard single-server shape without flattening arguments', () => {
    const parsed = parseMcpStdioConfig(JSON.stringify({
      mcpServers: {
        weather: {
          args: ['--directory', 'weather-my-mcp', 'run', 'weather.py'],
          command: 'uv',
        },
      },
    }));

    expect(parsed).toEqual({
      name: 'weather',
      command: 'uv',
      args: ['--directory', 'weather-my-mcp', 'run', 'weather.py'],
      env: {},
    });
  });

  it('round-trips environment variables for editing', () => {
    const raw = formatMcpStdioConfig({
      name: 'github',
      command: 'npx',
      args: ['-y', '@modelcontextprotocol/server-github'],
      env: { GITHUB_TOKEN: '********' },
    });

    expect(parseMcpStdioConfig(raw).env).toEqual({ GITHUB_TOKEN: '********' });
  });

  it('rejects multiple servers because one submit creates one registry entry', () => {
    expect(() => parseMcpStdioConfig(JSON.stringify({
      mcpServers: {
        first: { command: 'uvx', args: ['first'] },
        second: { command: 'uvx', args: ['second'] },
      },
    }))).toThrowError(McpStdioConfigError);
    try {
      parseMcpStdioConfig('{"mcpServers":{}}');
    } catch (error) {
      expect((error as McpStdioConfigError).code).toBe('singleServerRequired');
    }
  });
});
