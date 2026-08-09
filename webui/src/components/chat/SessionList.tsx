// ── Session list — shared between Chat, AgentChat, and Nl2sql ──────────────────

import { useEffect, useReducer, useState, useRef } from 'react';
import { Typography, Button, Dropdown, Input, Space } from 'antd';
import type { InputRef } from 'antd';
import {
  PlusOutlined,
  MoreOutlined,
  EditOutlined,
  DeleteOutlined,
  PushpinOutlined,
  PushpinFilled,
  StarOutlined,
  StarFilled,
  MessageOutlined,
} from '@ant-design/icons';
import type { MenuProps } from 'antd';
import type { SessionItem } from './types';
import { useTranslation } from 'react-i18next';

const { Text } = Typography;

interface SessionListProps {
  sessions: SessionItem[];
  activeSessionId: string | null;
  onSelect: (sessionId: string) => void;
  onNew: () => void;
  onDelete: (sessionId: string) => void;
  onRename: (sessionId: string, name: string) => void;
  onTogglePin: (sessionId: string) => void;
  onToggleBookmark?: (sessionId: string) => void;
  loading?: boolean;
  emptyText?: string;
}

function formatTime(ts: string | number | undefined): string {
  if (!ts) return '';
  const d = typeof ts === 'number' ? new Date(ts) : new Date(ts as string);
  const seconds = Math.floor((Date.now() - d.getTime()) / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

export interface SessionRenameUiState {
  menuOpen: boolean;
  renamePending: boolean;
  editing: boolean;
}

export type SessionRenameUiAction =
  | { type: 'menu'; open: boolean }
  | { type: 'selectRename' }
  | { type: 'activateRename' }
  | { type: 'finishRename' };

export const initialSessionRenameUiState: SessionRenameUiState = {
  menuOpen: false,
  renamePending: false,
  editing: false,
};

export function sessionRenameUiReducer(
  state: SessionRenameUiState,
  action: SessionRenameUiAction,
): SessionRenameUiState {
  switch (action.type) {
    case 'menu':
      return {
        ...state,
        menuOpen: action.open,
        renamePending: action.open ? false : state.renamePending,
      };
    case 'selectRename':
      return { menuOpen: false, renamePending: true, editing: false };
    case 'activateRename':
      return state.renamePending && !state.menuOpen
        ? { menuOpen: false, renamePending: false, editing: true }
        : state;
    case 'finishRename':
      return { menuOpen: false, renamePending: false, editing: false };
  }
}

function SessionItemRow({
  session,
  isActive,
  onClick,
  onDelete,
  onRename,
  onTogglePin,
  onToggleBookmark,
}: {
  session: SessionItem;
  isActive: boolean;
  onClick: () => void;
  onDelete: () => void;
  onRename: (id: string, name: string) => void;
  onTogglePin: () => void;
  onToggleBookmark?: () => void;
}) {
  const { t } = useTranslation();
  const [editName, setEditName] = useState(session.name);
  const [renameUi, dispatchRenameUi] = useReducer(
    sessionRenameUiReducer,
    initialSessionRenameUiState,
  );
  const inputRef = useRef<InputRef>(null);
  const renameFocusReadyRef = useRef(false);

  useEffect(() => {
    if (!renameUi.renamePending || renameUi.menuOpen) return undefined;
    const timer = window.setTimeout(() => {
      dispatchRenameUi({ type: 'activateRename' });
    }, 0);
    return () => window.clearTimeout(timer);
  }, [renameUi.menuOpen, renameUi.renamePending]);

  useEffect(() => {
    renameFocusReadyRef.current = false;
    if (!renameUi.editing) return undefined;
    // Dropdown restores focus to its trigger after closing. Let that lifecycle
    // finish before focusing the inline editor, otherwise the resulting blur
    // immediately commits and removes the input.
    const timer = window.setTimeout(() => {
      inputRef.current?.focus({ cursor: 'all' });
      renameFocusReadyRef.current = true;
    }, 80);
    return () => {
      window.clearTimeout(timer);
      renameFocusReadyRef.current = false;
    };
  }, [renameUi.editing]);

  const beginRename = () => {
    setEditName(session.name);
    dispatchRenameUi({ type: 'selectRename' });
  };

  const menuItems: MenuProps['items'] = [
    { key: 'rename', icon: <EditOutlined />, label: t('common.rename'), onClick: beginRename },
    { key: session.isPinned ? 'unpin' : 'pin', icon: session.isPinned ? <PushpinOutlined /> : <PushpinFilled />, label: session.isPinned ? t('common.unpin') : t('common.pin'), onClick: onTogglePin },
    ...(onToggleBookmark ? [{ key: 'bookmark', icon: session.isBookmarked ? <StarFilled style={{ color: '#faad14' }} /> : <StarOutlined />, label: session.isBookmarked ? t('common.unbookmark') : t('common.bookmark'), onClick: onToggleBookmark }] : []),
    { key: 'divider', type: 'divider' as const },
    { key: 'delete', icon: <DeleteOutlined />, label: t('common.deleteSession'), danger: true, onClick: onDelete },
  ];

  const commitRename = () => {
    if (editName.trim() && editName !== session.name) {
      onRename(session.sessionId, editName.trim());
    }
    renameFocusReadyRef.current = false;
    dispatchRenameUi({ type: 'finishRename' });
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') commitRename();
    if (e.key === 'Escape') {
      renameFocusReadyRef.current = false;
      dispatchRenameUi({ type: 'finishRename' });
    }
  };

  return (
    <div
      onClick={onClick}
      style={{
        padding: '10px 12px',
        cursor: 'pointer',
        background: isActive ? 'var(--session-active-bg)' : 'transparent',
        borderLeft: isActive ? '2px solid var(--session-active-border)' : '2px solid transparent',
        transition: 'all var(--transition-fast)',
        display: 'flex',
        alignItems: 'center',
        gap: 8,
      }}
      onMouseEnter={(e) => { if (!isActive) e.currentTarget.style.background = 'var(--bg-hover)'; }}
      onMouseLeave={(e) => { if (!isActive) e.currentTarget.style.background = 'transparent'; }}
    >
      <MessageOutlined style={{ fontSize: 12, color: 'var(--text-muted)', flexShrink: 0 }} />

      <div style={{ flex: 1, minWidth: 0 }}>
        {renameUi.editing ? (
          <Input
            ref={inputRef}
            size="small"
            value={editName}
            onChange={(e) => setEditName(e.target.value)}
            onKeyDown={handleKeyDown}
            onBlur={() => {
              if (renameFocusReadyRef.current) commitRename();
            }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            style={{ fontSize: 12 }}
          />
        ) : (
          <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
            {session.isPinned && (
              <span style={{ color: 'var(--accent-ai)', display: 'flex', alignItems: 'center' }}>
                <PushpinFilled style={{ fontSize: 10 }} />
              </span>
            )}
            {session.isBookmarked && (
              <span style={{ color: '#faad14', display: 'flex', alignItems: 'center' }}>
                <StarFilled style={{ fontSize: 10 }} />
              </span>
            )}
            <Text
              strong={isActive}
              style={{ fontSize: 13, display: 'block', color: 'var(--text-primary)' }}
              ellipsis
            >
              {session.name || session.sessionId.slice(0, 12)}
            </Text>
          </div>
        )}
        <Text type="secondary" style={{ fontSize: 10 }}>
          {session.state} · {formatTime(session.lastActivity ?? session.createdAt)}
        </Text>
      </div>

      <Dropdown
        menu={{ items: menuItems }}
        trigger={['click']}
        placement="bottomRight"
        open={renameUi.menuOpen}
        onOpenChange={(open) => {
          dispatchRenameUi({ type: 'menu', open });
        }}
      >
        <Button
          type="text"
          size="small"
          icon={<MoreOutlined />}
          onClick={(e) => e.stopPropagation()}
          style={{ color: 'var(--text-muted)', flexShrink: 0 }}
        />
      </Dropdown>
    </div>
  );
}

export function SessionList({
  sessions,
  activeSessionId,
  onSelect,
  onNew,
  onDelete,
  onRename,
  onTogglePin,
  onToggleBookmark,
  loading,
  emptyText = 'noSession',
}: SessionListProps) {
  const { t, i18n } = useTranslation();
  const sorted = [...sessions].sort((a, b) => {
    if (a.isBookmarked && !b.isBookmarked) return -1;
    if (!a.isBookmarked && b.isBookmarked) return 1;
    if (a.isPinned && !b.isPinned) return -1;
    if (!a.isPinned && b.isPinned) return 1;
    return new Date(b.lastActivity ?? b.createdAt).getTime() - new Date(a.lastActivity ?? a.createdAt).getTime();
  });

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      {/* New session button */}
      <div style={{ padding: '12px 12px 8px', display: 'flex' }}>
        <Button
          icon={<PlusOutlined />}
          onClick={onNew}
          style={{
            fontWeight: 600,
            width: 'calc(100% - 24px)',
            maxWidth: 176,
            minWidth: 132,
          }}
        >
          {t('chat.newSession')}
        </Button>
      </div>

      {/* Session list */}
      <div style={{ flex: 1, overflow: 'auto' }}>
        {loading ? (
          <div style={{ textAlign: 'center', padding: 24 }}>
            <Text type="secondary" style={{ fontSize: 12 }}>{t('common.loading')}</Text>
          </div>
        ) : sorted.length === 0 ? (
          <div style={{ padding: 16 }}>
            <Text type="secondary" style={{ fontSize: 12 }}>
              {i18n.exists(emptyText) ? t(emptyText) : emptyText}
            </Text>
          </div>
        ) : (
          sorted.map((session) => (
            <SessionItemRow
              key={session.sessionId}
              session={session}
              isActive={session.sessionId === activeSessionId}
              onClick={() => onSelect(session.sessionId)}
              onDelete={() => onDelete(session.sessionId)}
              onRename={onRename}
              onTogglePin={() => onTogglePin(session.sessionId)}
              onToggleBookmark={onToggleBookmark ? () => onToggleBookmark(session.sessionId) : undefined}
            />
          ))
        )}
      </div>
    </div>
  );
}
