import React from 'react';
import { Button, Input, Spin, Typography } from 'antd';
import { DownOutlined, UpOutlined } from '@ant-design/icons';
import { useState } from 'react';

const { Text } = Typography;

export interface PromptQueueItem {
  id: string;
  question: string;
  source?: string;
}

interface PromptQueuePanelProps {
  items: PromptQueueItem[];
  processing: boolean;
  activeItemId?: string | null;
  pendingText: string;
  labels: {
    title: string;
    processing: string;
    delete: string;
    inputPlaceholder: string;
    lockedHint: string;
    collapse: string;
    expand: string;
    retryTag?: string;
  };
  onUpdateItem: (id: string, question: string) => void;
  onDeleteItem: (id: string) => void;
}

export function PromptQueuePanel({
  items,
  processing,
  activeItemId,
  pendingText,
  labels,
  onUpdateItem,
  onDeleteItem,
}: PromptQueuePanelProps) {
  const [collapsed, setCollapsed] = useState(true);
  if (!processing && items.length === 0) return null;

  return (
    <div
      style={{
        marginTop: 8,
        border: '1px solid var(--border-subtle)',
        borderRadius: 10,
        background: 'rgba(59, 130, 246, 0.04)',
        padding: '10px',
        display: 'flex',
        flexDirection: 'column',
        gap: 8,
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <Text style={{ fontSize: 12, color: 'var(--text-primary)', fontWeight: 600 }}>
            {labels.title}
          </Text>
          <Text style={{ fontSize: 11, color: 'var(--text-secondary)' }}>
            {pendingText}
          </Text>
        </div>
        <Button
          type="text"
          size="small"
          icon={collapsed ? <DownOutlined /> : <UpOutlined />}
          onClick={() => setCollapsed((prev) => !prev)}
          style={{ color: 'var(--text-secondary)' }}
        >
          {collapsed ? labels.expand : labels.collapse}
        </Button>
      </div>

      {!collapsed && processing && (
        <Text style={{ fontSize: 11, color: 'var(--text-secondary)' }}>
          <Spin size="small" style={{ marginRight: 6 }} />
          {labels.processing}
        </Text>
      )}

      {!collapsed && (
        <Text style={{ fontSize: 11, color: 'var(--text-muted)' }}>
          {labels.lockedHint}
        </Text>
      )}

      {!collapsed && items.map((item, idx) => {
        const isActive = item.id === activeItemId;
        return (
          <div
            key={item.id}
            style={{
              border: '1px solid var(--border-subtle)',
              borderRadius: 8,
              padding: 8,
              background: isActive ? 'rgba(59, 130, 246, 0.09)' : 'var(--bg-elevated)',
              display: 'flex',
              flexDirection: 'column',
              gap: 6,
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
              <Text style={{ fontSize: 11, color: 'var(--text-secondary)' }}>
                {isActive ? labels.processing : `${idx + 1}.`}
              </Text>
              <Button
                type="text"
                size="small"
                disabled={isActive}
                onClick={() => onDeleteItem(item.id)}
                style={{ padding: '0 4px', height: 22, color: 'var(--text-muted)' }}
              >
                {labels.delete}
              </Button>
            </div>
            <Input.TextArea
              value={item.question}
              rows={2}
              disabled={isActive}
              onChange={(e) => onUpdateItem(item.id, e.target.value)}
              placeholder={labels.inputPlaceholder}
            />
            {item.source === 'retry' && !!labels.retryTag && (
              <Text style={{ fontSize: 10, color: 'var(--text-muted)' }}>
                {labels.retryTag}
              </Text>
            )}
          </div>
        );
      })}
    </div>
  );
}
