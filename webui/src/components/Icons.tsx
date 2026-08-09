/**
 * Unified SVG icon system for AOS Deep Space theme.
 * Replaces emoji icons with professional SVG equivalents.
 * All icons follow a consistent 16x16 grid with currentColor stroke styling.
 */

import type { CSSProperties, FC } from 'react';

const iconBase: CSSProperties = {
  flexShrink: 0,
  verticalAlign: 'middle',
};

const iconSizes = {
  xs: { width: 12, height: 12 },
  sm: { width: 14, height: 14 },
  md: { width: 16, height: 16 },
  lg: { width: 20, height: 20 },
  xl: { width: 24, height: 24 },
  xxl: { width: 32, height: 32 },
  giant: { width: 40, height: 40 },
} as const;

interface IconProps {
  size?: keyof typeof iconSizes;
  color?: string;
  className?: string;
  style?: CSSProperties;
}

const sizeStyle = (size: keyof typeof iconSizes, color?: string): CSSProperties => ({
  ...iconBase,
  ...iconSizes[size],
  color: color ?? 'currentColor',
});

// ── Status / Feedback ────────────────────────────────────────────────────────

/** Warning / alert icon (⚠️) */
export const AlertTriangleIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M8 1.5L14.5 13H1.5L8 1.5Z"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinejoin="round"
    />
    <path
      d="M8 6V9"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <circle cx="8" cy="11" r="0.75" fill="currentColor" />
  </svg>
);

/** Error / danger icon */
export const XCircleIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <circle cx="8" cy="8" r="6.5" stroke="currentColor" strokeWidth="1.25" />
    <path
      d="M5.5 5.5L10.5 10.5M10.5 5.5L5.5 10.5"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
  </svg>
);

/** Success / check icon (✅) */
export const CheckCircleIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <circle cx="8" cy="8" r="6.5" stroke="currentColor" strokeWidth="1.25" />
    <path
      d="M5.5 8L7 9.5L10.5 6"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

/** Info / neutral icon */
export const InfoIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <circle cx="8" cy="8" r="6.5" stroke="currentColor" strokeWidth="1.25" />
    <path
      d="M8 7V11"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <circle cx="8" cy="5" r="0.75" fill="currentColor" />
  </svg>
);

// ── Navigation / Actions ────────────────────────────────────────────────────

/** Search / magnifier icon (🔍 / 🔎) */
export const SearchIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <circle cx="7" cy="7" r="4.5" stroke="currentColor" strokeWidth="1.25" />
    <path
      d="M10.5 10.5L14 14"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    />
  </svg>
);

/** Folder / directory icon (📁) */
export const FolderIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M2 4.5C2 3.67 2.67 3 3.5 3H6L7.5 5H12.5C13.33 5 14 5.67 14 6.5V11.5C14 12.33 13.33 13 12.5 13H3.5C2.67 13 2 12.33 2 11.5V4.5Z"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinejoin="round"
    />
  </svg>
);

/** Clipboard / list icon (📋) */
export const ClipboardListIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <rect
      x="3.5"
      y="2.5"
      width="9"
      height="11"
      rx="1.5"
      stroke="currentColor"
      strokeWidth="1.25"
    />
    <path
      d="M6 2.5V2C6 1.17 6.67 0.5 7.5 0.5C8.33 0.5 9 1.17 9 2V2.5"
      stroke="currentColor"
      strokeWidth="1.25"
    />
    <path
      d="M5.5 6H10.5M5.5 8.5H10.5M5.5 11H8"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
  </svg>
);

/** Chat / message icon (💬) */
export const MessageIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M2 3.5C2 2.67 2.67 2 3.5 2H12.5C13.33 2 14 2.67 14 3.5V10C14 10.83 13.33 11.5 12.5 11.5H7L4 14V11.5H3.5C2.67 11.5 2 10.83 2 10V3.5Z"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinejoin="round"
    />
    <path
      d="M5 6H11M5 8H9"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
  </svg>
);

/** Database / storage icon (🗄️) */
export const DatabaseIcon: FC<IconProps> = ({ size = 'md', color, className, style }) => (
  <svg
    className={className}
    style={{ ...sizeStyle(size, color), ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <ellipse cx="8" cy="3.5" rx="5" ry="2" stroke="currentColor" strokeWidth="1.25" />
    <path
      d="M3 3.5V7C3 8.66 5.34 10 8 10C10.66 10 13 8.66 13 7V3.5"
      stroke="currentColor"
      strokeWidth="1.25"
    />
    <path
      d="M3 7V10.5C3 12.16 5.34 13.5 8 13.5C10.66 13.5 13 12.16 13 10.5V7"
      stroke="currentColor"
      strokeWidth="1.25"
    />
    <path
      d="M3 10.5V12.5C3 13.16 5.34 13.5 8 13.5C10.66 13.5 13 13.16 13 12.5V10.5"
      stroke="currentColor"
      strokeWidth="1.25"
    />
  </svg>
);

// ── Re-export existing stage icons ──────────────────────────────────────────

export {
  IdleIcon,
  DiscoveringIcon,
  ThinkingIcon,
  PlanIcon,
  ReviewIcon,
  PipelineIcon,
  PinIcon,
  LightbulbIcon,
  RocketIcon,
} from './StageIcon';
