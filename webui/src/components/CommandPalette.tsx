import { useState, useEffect, useRef } from 'react';
import { Input, Typography, Space, Divider } from 'antd';
import {
  MessageOutlined,
  RobotOutlined,
  RocketOutlined,
  DashboardOutlined,
  FolderOpenOutlined,
} from '@ant-design/icons';
import { SearchIcon, ClipboardListIcon } from './Icons';
import { useNavigate } from '@/router';
import type { MenuProps } from 'antd';
import { useTranslation } from 'react-i18next';

const { Text } = Typography;

interface CommandItem {
  id: string;
  icon: React.ReactNode;
  label: string;
  sublabel?: string;
  shortcut?: string[];
  group: string;
  action: () => void;
}

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
}

export function CommandPalette({ open, onClose }: CommandPaletteProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const navigate = useNavigate();

  useEffect(() => {
    if (open) {
      setQuery('');
      setSelected(0);
      setTimeout(() => inputRef.current?.focus(), 50);
    }
  }, [open]);

  const QUICK_ACTIONS: CommandItem[] = [
    { id: 'quick-repair', icon: <RocketOutlined />, label: t('commandPalette.quickRepair'), sublabel: t('commandPalette.quickRepairDesc'), group: t('commandPalette.quickActions'), action: () => {} },
    { id: 'quick-feature', icon: <RobotOutlined />, label: t('commandPalette.quickFeature'), sublabel: t('commandPalette.quickFeatureDesc'), group: t('commandPalette.quickActions'), action: () => {} },
    { id: 'quick-review', icon: <MessageOutlined />, label: t('commandPalette.quickReview'), sublabel: t('commandPalette.quickReviewDesc'), group: t('commandPalette.quickActions'), action: () => {} },
  ];

  const NAV_ITEMS: CommandItem[] = [
    { id: 'nav-dashboard', icon: <DashboardOutlined />, label: t('nav.dashboard'), sublabel: t('commandPalette.goToDashboard'), shortcut: ['G', 'D'], group: t('commandPalette.navigate'), action: () => {} },
    { id: 'nav-agent', icon: <RobotOutlined />, label: t('nav.agent'), sublabel: t('commandPalette.goToAgent'), shortcut: ['G', 'A'], group: t('commandPalette.navigate'), action: () => {} },
    { id: 'nav-pipeline', icon: <RocketOutlined />, label: t('nav.pipeline'), sublabel: t('commandPalette.goToPipeline'), group: t('commandPalette.navigate'), action: () => {} },
    { id: 'nav-projects', icon: <FolderOpenOutlined />, label: t('nav.projects'), sublabel: t('commandPalette.goToProjects'), group: t('commandPalette.navigate'), action: () => {} },
  ];

  const RECENT_TASKS = [
    '为 user-service 添加 OAuth2 认证模块',
    '修复 payment-api 的超时问题',
    '重构 inventory 微服务的数据库访问层',
  ];

  const allItems: CommandItem[] = [
    ...QUICK_ACTIONS,
    ...NAV_ITEMS.map((item) => ({
      ...item,
      action: () => {
        navigate(item.id.replace('nav-', '/'));
        onClose();
      },
    })),
  ];

  const filtered = query.trim()
    ? allItems.filter(
        (item) =>
          item.label.toLowerCase().includes(query.toLowerCase()) ||
          (item.sublabel ?? '').toLowerCase().includes(query.toLowerCase())
      )
    : allItems;

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setSelected((s) => Math.min(s + 1, filtered.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setSelected((s) => Math.max(s - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      filtered[selected]?.action();
      onClose();
    } else if (e.key === 'Escape') {
      onClose();
    }
  };

  if (!open) return null;

  // Group items
  const groups: Record<string, CommandItem[]> = {};
  for (const item of filtered) {
    if (!groups[item.group]) groups[item.group] = [];
    groups[item.group].push(item);
  }

  let globalIndex = -1;

  return (
    <div
      style={{
        position: 'fixed',
        inset: 0,
        zIndex: 1000,
        display: 'flex',
        alignItems: 'flex-start',
        justifyContent: 'center',
        paddingTop: 80,
        background: 'rgba(8, 12, 20, 0.8)',
        backdropFilter: 'blur(8px)',
      }}
      onClick={onClose}
    >
      <div
        style={{
          width: '100%',
          maxWidth: 600,
          background: 'var(--bg-elevated)',
          border: '1px solid var(--border-default)',
          borderRadius: 12,
          boxShadow: '0 24px 64px rgba(0,0,0,0.5), 0 0 0 1px rgba(124,58,237,0.1)',
          overflow: 'hidden',
          animation: 'slideUp 180ms ease forwards',
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Input */}
        <div style={{ padding: '12px 16px', borderBottom: '1px solid var(--border-subtle)', display: 'flex', alignItems: 'center', gap: 12 }}>
          <SearchIcon size="lg" />
          <input
            ref={inputRef as React.RefObject<HTMLInputElement>}
            value={query}
            onChange={(e) => { setQuery(e.target.value); setSelected(0); }}
            onKeyDown={handleKeyDown}
            placeholder={t('commandPalette.searchCommands')}
            style={{
              flex: 1,
              background: 'transparent',
              border: 'none',
              outline: 'none',
              color: 'var(--text-primary)',
              fontSize: 15,
              fontFamily: 'var(--font-ui)',
            }}
          />
          <kbd style={{
            background: 'var(--bg-interactive)',
            border: '1px solid var(--border-default)',
            borderRadius: 4,
            padding: '2px 6px',
            fontSize: 11,
            color: 'var(--text-muted)',
            fontFamily: 'var(--font-code)',
          }}>ESC</kbd>
        </div>

        {/* Results */}
        <div style={{ maxHeight: 420, overflowY: 'auto' }}>
          {/* Quick actions (only when no query) */}
          {!query && (
            <div>
              <div style={{ padding: '8px 16px 4px', fontSize: 11, color: 'var(--text-muted)', fontWeight: 600, textTransform: 'uppercase', letterSpacing: '0.08em' }}>
                {t('commandPalette.quickActions')}
              </div>
              {QUICK_ACTIONS.map((item) => {
                globalIndex++;
                const idx = globalIndex;
                return (
                  <div
                    key={item.id}
                    onClick={() => { item.action(); onClose(); }}
                    style={{
                      padding: '10px 16px',
                      display: 'flex',
                      alignItems: 'center',
                      gap: 12,
                      cursor: 'pointer',
                      background: selected === idx ? 'var(--accent-ai-muted)' : 'transparent',
                      transition: 'background var(--transition-fast)',
                    }}
                    onMouseEnter={() => setSelected(idx)}
                  >
                    <span style={{ fontSize: 16 }}>{item.icon}</span>
                    <div style={{ flex: 1 }}>
                      <Text style={{ fontSize: 14, color: 'var(--text-primary)' }}>{item.label}</Text>
                      <Text style={{ fontSize: 12, color: 'var(--text-secondary)', display: 'block' }}>{item.sublabel}</Text>
                    </div>
                  </div>
                );
              })}

              <div style={{ padding: '8px 16px 4px', fontSize: 11, color: 'var(--text-muted)', fontWeight: 600, textTransform: 'uppercase', letterSpacing: '0.08em' }}>
                {t('commandPalette.recentTasks')}
              </div>
              {RECENT_TASKS.map((task) => {
                globalIndex++;
                const idx = globalIndex;
                return (
                  <div
                    key={task}
                    onClick={() => onClose()}
                    style={{
                      padding: '8px 16px',
                      display: 'flex',
                      alignItems: 'center',
                      gap: 10,
                      cursor: 'pointer',
                      background: selected === idx ? 'var(--accent-ai-muted)' : 'transparent',
                      transition: 'background var(--transition-fast)',
                      borderLeft: '2px solid transparent',
                    }}
                    onMouseEnter={() => setSelected(idx)}
                  >
                    <ClipboardListIcon size="xs" color="var(--text-muted)" />
                    <Text style={{ fontSize: 13, color: 'var(--text-secondary)' }}>{task}</Text>
                  </div>
                );
              })}
            </div>
          )}

          {/* Filtered results */}
          {query && Object.entries(groups).map(([group, items]) => (
            <div key={group}>
              <div style={{ padding: '8px 16px 4px', fontSize: 11, color: 'var(--text-muted)', fontWeight: 600, textTransform: 'uppercase', letterSpacing: '0.08em' }}>
                {group}
              </div>
              {items.map((item) => {
                globalIndex++;
                const idx = globalIndex;
                return (
                  <div
                    key={item.id}
                    onClick={() => { item.action(); onClose(); }}
                    style={{
                      padding: '10px 16px',
                      display: 'flex',
                      alignItems: 'center',
                      gap: 12,
                      cursor: 'pointer',
                      background: selected === idx ? 'var(--accent-ai-muted)' : 'transparent',
                      transition: 'background var(--transition-fast)',
                      borderLeft: selected === idx ? '2px solid var(--accent-ai)' : '2px solid transparent',
                    }}
                    onMouseEnter={() => setSelected(idx)}
                  >
                    <span style={{ fontSize: 16 }}>{item.icon}</span>
                    <div style={{ flex: 1 }}>
                      <Text style={{ fontSize: 14, color: 'var(--text-primary)' }}>{item.label}</Text>
                      {item.sublabel && (
                        <Text style={{ fontSize: 12, color: 'var(--text-secondary)', display: 'block' }}>{item.sublabel}</Text>
                      )}
                    </div>
                    {item.shortcut && (
                      <Space size={4}>
                        {item.shortcut.map((k) => (
                          <kbd key={k} style={{
                            background: 'var(--bg-interactive)',
                            border: '1px solid var(--border-default)',
                            borderRadius: 3,
                            padding: '1px 5px',
                            fontSize: 10,
                            color: 'var(--text-muted)',
                            fontFamily: 'var(--font-code)',
                          }}>{k}</kbd>
                        ))}
                      </Space>
                    )}
                  </div>
                );
              })}
            </div>
          ))}

          {/* Empty */}
          {query && filtered.length === 0 && (
            <div style={{ padding: '32px 16px', textAlign: 'center' }}>
              <Text style={{ color: 'var(--text-muted)' }}>{t('commandPalette.noResults')}</Text>
            </div>
          )}
        </div>

        {/* Footer */}
        <div style={{
          padding: '8px 16px',
          borderTop: '1px solid var(--border-subtle)',
          display: 'flex',
          alignItems: 'center',
          gap: 16,
          fontSize: 11,
          color: 'var(--text-muted)',
        }}>
          <Space size={4}>
            <kbd style={{ background: 'var(--bg-interactive)', border: '1px solid var(--border-default)', borderRadius: 3, padding: '1px 5px', fontSize: 10 }}>↑↓</kbd>
            <span>{t('commandPalette.navigate')}</span>
          </Space>
          <Space size={4}>
            <kbd style={{ background: 'var(--bg-interactive)', border: '1px solid var(--border-default)', borderRadius: 3, padding: '1px 5px', fontSize: 10 }}>↵</kbd>
            <span>{t('commandPalette.select')}</span>
          </Space>
          <Space size={4}>
            <kbd style={{ background: 'var(--bg-interactive)', border: '1px solid var(--border-default)', borderRadius: 3, padding: '1px 5px', fontSize: 10 }}>ESC</kbd>
            <span>{t('commandPalette.close')}</span>
          </Space>
        </div>
      </div>
    </div>
  );
}
