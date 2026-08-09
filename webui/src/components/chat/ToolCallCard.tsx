// ── Tool call card — shared between Chat and AgentChat ─────────────────────────

import { useState } from 'react';
import { Typography, Tag, Space } from 'antd';
import {
  CheckCircleOutlined,
  CloseCircleOutlined,
  LoadingOutlined,
  ToolOutlined,
  RightOutlined,
  DownOutlined,
} from '@ant-design/icons';
import type { ToolCallInfo } from './types';

const { Text } = Typography;

interface ToolCallCardProps {
  tool: ToolCallInfo;
  defaultExpanded?: boolean;
  showSource?: boolean;
}

export function ToolCallCard({
  tool,
  defaultExpanded = false,
  showSource = true,
}: ToolCallCardProps) {
  const [expanded, setExpanded] = useState(defaultExpanded);

  const borderColor = tool.isError ? 'var(--color-error)' : 'rgba(88,166,255,0.3)';
  const bgColor = tool.isError ? 'rgba(255,77,79,0.04)' : 'rgba(24,144,255,0.04)';
  const headerBg = tool.isError ? 'rgba(255,77,79,0.08)' : 'rgba(24,144,255,0.08)';

  const tryParse = (raw: string): string => {
    if (!raw) return '';
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      return raw;
    }
  };

  const parsedArgs = tryParse(tool.args);
  const parsedResult = tryParse(tool.result);
  const hasBody = !!(parsedArgs || parsedResult);

  return (
    <div
      style={{
        border: `1px solid ${borderColor}`,
        borderRadius: 8,
        marginTop: 6,
        overflow: 'hidden',
        background: bgColor,
      }}
    >
      <div
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 8,
          padding: '6px 12px',
          background: headerBg,
          cursor: hasBody ? 'pointer' : 'default',
        }}
        onClick={hasBody ? () => setExpanded((v) => !v) : undefined}
      >
        {tool.status === 'success' ? (
          <CheckCircleOutlined style={{ color: 'var(--color-success)', fontSize: 13 }} />
        ) : tool.status === 'error' ? (
          <CloseCircleOutlined style={{ color: 'var(--color-error)', fontSize: 13 }} />
        ) : tool.status === 'running' ? (
          <LoadingOutlined style={{ color: 'var(--accent-ai)', fontSize: 13 }} />
        ) : (
          <ToolOutlined style={{ color: 'var(--text-muted)', fontSize: 13 }} />
        )}

        <Text
          code
          style={{
            fontSize: 11,
            background: 'transparent',
            padding: 0,
            color: tool.isError ? 'var(--color-error)' : 'var(--accent-ai)',
          }}
        >
          {tool.name}
        </Text>

        {showSource && tool.source === 'mcp' && tool.mcpServer && (
          <Tag color="purple" style={{ fontSize: 10 }}>MCP: {tool.mcpServer}</Tag>
        )}
        {showSource && tool.source === 'builtin' && (
          <Tag color="blue" style={{ fontSize: 10 }}>{'builtin'}</Tag>
        )}
        {showSource && tool.source === 'skill' && (
          <Tag color="gold" style={{ fontSize: 10 }}>
            {tool.skillName ? `Skill: ${tool.skillName}` : 'Skill'}
          </Tag>
        )}

        {tool.status === 'pending' && (
          <Tag color="processing" style={{ fontSize: 10, marginLeft: 'auto' }}>
            <LoadingOutlined spin /> {'toolPending'}
          </Tag>
        )}
        {(tool.status === 'success' || tool.status === 'error') && tool.durationMs != null && (
          <Text style={{ fontSize: 11, color: 'var(--text-secondary)', marginLeft: 'auto' }}>
            {tool.durationMs}ms
          </Text>
        )}
        {hasBody && (
          <span style={{ marginLeft: 4, color: 'var(--text-muted)', fontSize: 10 }}>
            {expanded ? <DownOutlined style={{ fontSize: 10 }} /> : <RightOutlined style={{ fontSize: 10 }} />}
          </span>
        )}
      </div>

      {hasBody && expanded && (
        <div style={{ padding: '8px 12px', borderTop: `1px solid ${borderColor}` }}>
          {parsedArgs && (
            <div style={{ marginBottom: 8 }}>
              <Text
                style={{
                  fontSize: 10,
                  color: 'var(--text-secondary)',
                  textTransform: 'uppercase',
                  letterSpacing: '0.5px',
                  display: 'block',
                  marginBottom: 4,
                }}
              >
                {'toolArgs'}
              </Text>
              <pre
                style={{
                  margin: 0,
                  fontSize: 12,
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-all',
                  color: 'var(--text-secondary)',
                  background: 'var(--bg-interactive)',
                  padding: 8,
                  borderRadius: 4,
                  maxHeight: 200,
                  overflow: 'auto',
                }}
              >
                {parsedArgs}
              </pre>
            </div>
          )}
          {parsedResult && (
            <div>
              <Text
                style={{
                  fontSize: 10,
                  color: 'var(--text-secondary)',
                  textTransform: 'uppercase',
                  letterSpacing: '0.5px',
                  display: 'block',
                  marginBottom: 4,
                }}
              >
                {'toolResult'}
              </Text>
              <pre
                style={{
                  margin: 0,
                  fontSize: 12,
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-all',
                  color: tool.isError ? 'var(--color-error)' : 'var(--text-secondary)',
                  background: 'var(--bg-interactive)',
                  padding: 8,
                  borderRadius: 4,
                  maxHeight: 300,
                  overflow: 'auto',
                }}
              >
                {parsedResult}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** Compact inline tool call badges row */
export function ToolCallBadgeRow({ toolCalls }: { toolCalls: ToolCallInfo[] }) {
  if (!toolCalls.length) return null;
  return (
    <Space size={4} wrap>
      {toolCalls.map((tc) => (
        <Tag
          key={tc.index}
          color={tc.isError ? 'red' : tc.status === 'running' ? 'processing' : 'green'}
          style={{ fontSize: 10, display: 'flex', alignItems: 'center', gap: 2 }}
        >
          {tc.status === 'running' ? (
            <LoadingOutlined spin style={{ fontSize: 10 }} />
          ) : tc.isError ? (
            <CloseCircleOutlined style={{ fontSize: 10 }} />
          ) : (
            <CheckCircleOutlined style={{ fontSize: 10 }} />
          )}
          {tc.name}
        </Tag>
      ))}
    </Space>
  );
}
