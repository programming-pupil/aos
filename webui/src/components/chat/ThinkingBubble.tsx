// ── Thinking bubble — the collapsible reasoning panel used in chat ─────────
//
// Rendered both during streaming (live deltas, spinning icon, "思考中…")
// and on the persisted assistant message (static check, "已深度思考（Xs）").
// This is the *only* thinking bubble in the app — the previous copies
// at `components/ThinkingBubble.tsx` and `components/chat/ThinkingBubble.tsx`
// were superseded by this one to eliminate three near-duplicate widgets
// drifting out of sync.
//
// UX contract:
//   • while `loading=true` or `text==''`: spinner + "思考中…" label
//   • once `loading=false` AND we have text: static icon + "已深度思考"
//     (+ duration when `durationMs` is known). The bubble remains
//     clickable and the content collapsible so users can re-read the
//     model's reasoning — exactly the DeepSeek-style UX the project
//     asked for.
//   • `text` is a streaming single-line preview when collapsed; the full
//     content is shown in a scrollable monospace panel when expanded.

import { Typography } from 'antd';
import {
  CaretRightOutlined,
  CheckCircleFilled,
  Loading3QuartersOutlined,
} from '@ant-design/icons';
import type { useTranslation } from 'react-i18next';

const { Text } = Typography;

export interface ThinkingBubbleProps {
  /** The accumulated reasoning text. Empty while the stream is still ramping up. */
  text: string;
  /** Whether the expanded panel is open. Controlled by the parent. */
  expanded: boolean;
  /** Callback when the header is clicked. */
  onToggle?: () => void;
  /**
   * True while the reasoning stream is still active. False (or undefined)
   * once `thinking_end` has fired, the first `text_delta` closed the
   * block, or the message has been persisted.
   */
  loading?: boolean;
  /**
   * Wall-clock duration of the reasoning stream, in milliseconds.
   * Rendered as "X.Xs" next to the "已深度思考" label so users can see
   * how long the model reasoned — matches the DeepSeek web UI.
   */
  durationMs?: number;
  /** Translator handle — passed through so the bubble respects i18n. */
  t?: ReturnType<typeof useTranslation>['t'];
}

/**
 * Format a millisecond duration as a short, human-readable string.
 * `< 1s` → "Xms"; 1s–59s → "X.Xs"; ≥ 1 min → "Xm Ys".
 */
function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) {
    return '';
  }
  if (ms < 1_000) {
    return `${Math.round(ms)}ms`;
  }
  const totalSeconds = ms / 1_000;
  if (totalSeconds < 60) {
    // One decimal place reads cleanly without being jittery.
    return `${totalSeconds.toFixed(1)}s`;
  }
  const mins = Math.floor(totalSeconds / 60);
  const secs = Math.round(totalSeconds - mins * 60);
  return `${mins}m ${secs}s`;
}

export function ThinkingBubble({
  text,
  expanded,
  onToggle,
  loading,
  durationMs,
  t,
}: ThinkingBubbleProps) {
  // Don't render an empty bubble. `loading && !text` is the "just started
  // reasoning" case — we *do* want to render that so users see immediate
  // feedback; we just skip the fully-empty state.
  if (!text && !loading) {
    return null;
  }

  // "Done" state requires BOTH (a) we're no longer marked loading and
  // (b) we actually captured some reasoning text. A persisted assistant
  // message with `thinking: '…content…'` and no `thinkingLoading` always
  // qualifies. This is the state the user reported as broken: the old
  // bubble had no concept of "done" and always showed "思考中" with a
  // loader icon.
  const isDone = !loading && !!text;
  const label = t ? t(isDone ? 'chat.thoughtFor' : 'chat.thinking') : (isDone ? '已深度思考' : '思考中…');
  const durationLabel = isDone && durationMs !== undefined ? formatDuration(durationMs) : '';

  return (
    <div
      style={{
        background: 'rgba(124,58,237,0.06)',
        border: '1px solid rgba(124,58,237,0.2)',
        borderRadius: 10,
        marginBottom: 10,
        overflow: 'hidden',
      }}
    >
      <div
        onClick={onToggle}
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: 6,
          padding: '7px 12px',
          cursor: onToggle ? 'pointer' : 'default',
          userSelect: 'none',
        }}
      >
        {isDone ? (
          <CheckCircleFilled
            style={{ fontSize: 11, color: 'var(--accent-ai)', flexShrink: 0 }}
          />
        ) : (
          <Loading3QuartersOutlined
            spin
            style={{ fontSize: 11, color: 'var(--accent-ai)', flexShrink: 0 }}
          />
        )}
        <Text style={{ fontSize: 12, color: 'var(--text-secondary)', flexShrink: 0 }}>
          {label}
        </Text>
        {durationLabel && (
          <Text style={{ fontSize: 11, color: 'var(--text-muted)', flexShrink: 0 }}>
            {`(${durationLabel})`}
          </Text>
        )}
        {text && (
          <Text
            style={{ fontSize: 11, color: 'var(--text-muted)', flexShrink: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
          >
            {`· ${text.length > 60 ? text.slice(0, 60) + '…' : text}`}
          </Text>
        )}
        <span style={{ marginLeft: 'auto', flexShrink: 0, paddingLeft: 8 }}>
          <CaretRightOutlined
            style={{
              fontSize: 10,
              color: 'var(--text-secondary)',
              transform: expanded ? 'rotate(90deg)' : 'rotate(0deg)',
              transition: 'transform 0.2s',
            }}
          />
        </span>
      </div>
      {expanded && (
        <div
          style={{
            padding: '8px 14px 10px',
            borderTop: '1px solid rgba(124,58,237,0.12)',
            fontSize: 13,
            lineHeight: 1.7,
            color: 'var(--text-secondary)',
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-word',
            fontFamily: "'JetBrains Mono', monospace",
            maxHeight: 320,
            overflow: 'auto',
          }}
        >
          {text || (
            <Text style={{ fontSize: 12, color: 'var(--text-secondary)', fontStyle: 'italic' }}>
              <Loading3QuartersOutlined spin style={{ marginRight: 4 }} />
              {t ? t('chat.analyzing') : 'analyzing'}
            </Text>
          )}
        </div>
      )}
    </div>
  );
}
