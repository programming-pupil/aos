// ── Token usage bar — shared between all chat pages ─────────────────────────────────

import { Typography, Tag, Space } from 'antd';

const { Text } = Typography;

interface TokenUsageBarProps {
  inputTokens: number;
  outputTokens: number;
  estimatedCostUsd?: number;
  model?: string;
  compact?: boolean;
}

export function TokenUsageBar({
  inputTokens,
  outputTokens,
  estimatedCostUsd,
  model,
  compact,
}: TokenUsageBarProps) {
  const total = inputTokens + outputTokens;

  const formatK = (n: number) => n >= 1000 ? `${(n / 1000).toFixed(1)}K` : String(n);

  if (compact) {
    return (
      <Space size={4}>
        <Tag color="cyan" style={{ fontSize: 11 }}>⬆ {formatK(inputTokens)}</Tag>
        <Tag color="green" style={{ fontSize: 11 }}>⬇ {formatK(outputTokens)}</Tag>
        {estimatedCostUsd != null && (
          <Tag color="purple" style={{ fontSize: 11 }}>$ {estimatedCostUsd.toFixed(4)}</Tag>
        )}
      </Space>
    );
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6, padding: '8px 12px', background: 'var(--bg-elevated)', borderRadius: 8 }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
        <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
          Tokens: {formatK(total)}
          {model ? ` · ${model}` : ''}
        </Text>
      </div>
      <div style={{ display: 'flex', gap: 12 }}>
        <Tag color="cyan" style={{ fontSize: 11 }}>⬆ In: {formatK(inputTokens)}</Tag>
        <Tag color="green" style={{ fontSize: 11 }}>⬇ Out: {formatK(outputTokens)}</Tag>
        {estimatedCostUsd != null && (
          <Tag color="purple" style={{ fontSize: 11 }}>$ {estimatedCostUsd.toFixed(4)}</Tag>
        )}
      </div>
    </div>
  );
}
