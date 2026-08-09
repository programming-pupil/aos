// ── Slash command panel — shared between Chat and AgentChat ────────────────────

import { useEffect, useRef } from 'react';
import { Typography } from 'antd';
import {
  SearchOutlined,
  ApiOutlined,
  ThunderboltOutlined,
  RocketOutlined,
} from '@ant-design/icons';
import type { SlashCommandDef } from './types';
import { distance } from 'fastest-levenshtein';

const { Text } = Typography;

function fuzzyScore(query: string, target: string): number {
  const q = query.toLowerCase().trim();
  const t = target.toLowerCase();
  if (t === q) return 0;
  if (t.startsWith(q)) return 1;
  if (t.includes(q)) return 2;
  const dist = distance(q, t);
  return dist <= 3 ? 3 : Infinity;
}

function filterCommands(commands: SlashCommandDef[], filter: string): SlashCommandDef[] {
  if (!filter) return commands;
  const scored = commands
    .map((cmd) => {
      const nameScore = fuzzyScore(filter, cmd.name);
      const descScore = fuzzyScore(filter, cmd.description);
      const hintScore = cmd.hint ? fuzzyScore(filter, cmd.hint) : Infinity;
      return { cmd, score: Math.min(nameScore, descScore, hintScore) };
    })
    .filter(({ score }) => score < Infinity)
    .sort((a, b) => a.score - b.score);
  return scored.map(({ cmd }) => cmd);
}

interface SlashCommandPanelProps {
  commands: SlashCommandDef[];
  filter: string;
  selectedIndex: number;
  onSelect: (cmd: SlashCommandDef) => void;
  onHover: (index: number) => void;
  onClose: () => void;
  containerRef?: React.RefObject<HTMLDivElement>;
}

export function SlashCommandPanel({
  commands,
  filter,
  selectedIndex,
  onSelect,
  onHover,
  onClose,
  containerRef,
}: SlashCommandPanelProps) {
  const filtered = filterCommands(commands, filter);
  const selectedRef = useRef<HTMLDivElement>(null);

  // Auto-scroll selected item into view
  useEffect(() => {
    selectedRef.current?.scrollIntoView({ block: 'nearest' });
  }, [selectedIndex]);

  const categoryIcons: Record<string, React.ReactNode> = {
    builtin: <ThunderboltOutlined style={{ fontSize: 12, color: 'var(--accent-ai)' }} />,
    skill: <RocketOutlined style={{ fontSize: 12, color: '#faad14' }} />,
    mcp: <ApiOutlined style={{ fontSize: 12, color: '#a855f7' }} />,
  };

  const categoryColors: Record<string, string> = {
    builtin: 'var(--accent-ai)',
    skill: '#faad14',
    mcp: '#a855f7',
  };

  return (
    <div
      style={{
        border: '1px solid var(--border-default)',
        borderRadius: 10,
        background: 'var(--bg-elevated)',
        boxShadow: '0 8px 24px rgba(0,0,0,0.2)',
        marginBottom: 8,
        maxHeight: 320,
        display: 'flex',
        flexDirection: 'column',
        zIndex: 1000,
      }}
    >
      {/* Header */}
      <div
        style={{
          padding: '6px 12px',
          borderBottom: '1px solid var(--border-subtle)',
          background: 'rgba(124,58,237,0.08)',
          display: 'flex',
          alignItems: 'center',
          gap: 8,
        }}
      >
        <SearchOutlined style={{ fontSize: 12, color: 'var(--accent-ai)' }} />
        <Text
          style={{
            fontSize: 12,
            color: 'var(--accent-ai)',
            fontFamily: 'var(--font-code)',
          }}
        >
          /{filter || 'commands'}
        </Text>
        <Text style={{ fontSize: 11, color: 'var(--text-muted)', marginLeft: 'auto' }}>
          ↑↓ navigate · Enter select · Esc close
        </Text>
      </div>

      {/* List */}
      <div style={{ overflow: 'auto', flex: 1 }}>
        {filtered.length === 0 ? (
          <div style={{ padding: '16px 12px', textAlign: 'center' }}>
            <Text style={{ fontSize: 13, color: 'var(--text-secondary)' }}>
              No matching slash command
            </Text>
          </div>
        ) : (
          filtered.map((cmd, i) => (
            <div
              key={cmd.name}
              ref={i === selectedIndex ? selectedRef : undefined}
              onClick={() => onSelect(cmd)}
              onMouseEnter={() => onHover(i)}
              style={{
                padding: '8px 12px',
                cursor: 'pointer',
                background: i === selectedIndex ? 'rgba(124,58,237,0.12)' : 'transparent',
                borderLeft: i === selectedIndex ? '2px solid var(--accent-ai)' : '2px solid transparent',
                display: 'flex',
                flexDirection: 'column',
                gap: 2,
                transition: 'background 0.1s',
              }}
            >
              <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <span style={{ color: categoryColors[cmd.source], display: 'flex', alignItems: 'center' }}>
                  {categoryIcons[cmd.source]}
                </span>
                <Text
                  style={{
                    fontSize: 13,
                    fontFamily: 'var(--font-code)',
                    color: categoryColors[cmd.source],
                  }}
                >
                  /{cmd.name}
                </Text>
                {cmd.hint && (
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {cmd.hint.replace(/^\/[^\s]+\s*/, '')}
                  </Text>
                )}
              </div>
              <Text type="secondary" style={{ fontSize: 12, marginLeft: 24 }}>
                {cmd.description}
              </Text>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
