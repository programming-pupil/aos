/**
 * Stage indicator SVG icons for Agent Chat.
 * Each icon is designed for the AOS Deep Space theme at 16x16px.
 */

import type { CSSProperties, FC } from 'react';

const iconBase: CSSProperties = {
  width: 14,
  height: 14,
  flexShrink: 0,
  verticalAlign: 'middle',
};

interface StageIconProps {
  className?: string;
  style?: CSSProperties;
}

export const IdleIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ ...iconBase, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M8 1.5C4.41 1.5 1.5 4.41 1.5 8C1.5 11.59 4.41 14.5 8 14.5C11.59 14.5 14.5 11.59 14.5 8C14.5 4.41 11.59 1.5 8 1.5Z"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
    <path
      d="M8 5V8.5L10.5 10"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

export const DiscoveringIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ ...iconBase, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <circle
      cx="7"
      cy="7"
      r="4.5"
      stroke="currentColor"
      strokeWidth="1.25"
    />
    <path
      d="M10.5 10.5L13.5 13.5"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    />
  </svg>
);

export const ThinkingIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ ...iconBase, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M8 1.5C4.41 1.5 1.5 4.41 1.5 8C1.5 11.59 4.41 14.5 8 14.5C11.59 14.5 14.5 11.59 14.5 8"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <path
      d="M8 5.5C6.07 5.5 4.5 7.07 4.5 9C4.5 10.93 6.07 12.5 8 12.5"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <path
      d="M11 3.5C10.17 2.67 9.14 2.1 8 1.9"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <circle cx="8" cy="8" r="1" fill="currentColor" />
  </svg>
);

export const PlanIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ ...iconBase, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <rect
      x="2"
      y="2"
      width="12"
      height="12"
      rx="2"
      stroke="currentColor"
      strokeWidth="1.25"
    />
    <path
      d="M5 5.5H11M5 8H11M5 10.5H8.5"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
  </svg>
);

export const ReviewIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ ...iconBase, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M1.5 8C1.5 4.41 4.41 1.5 8 1.5C10.21 1.5 12.2 2.69 13.4 4.5"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <path
      d="M13.4 4.5C14.1 5.4 14.5 6.5 14.5 7.7C14.5 10.5 12.25 12.8 9.5 13C9.03 13.05 8.55 13.05 8.08 13"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <path
      d="M9.5 9.5L11.5 11.5L14.5 7.5"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
);

export const PipelineIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ ...iconBase, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M2.5 8H5.5M7.5 8H10.5M12.5 8H13.5"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    />
    <rect
      x="2.5"
      y="3.5"
      width="3"
      height="3"
      rx="0.75"
      stroke="currentColor"
      strokeWidth="1.1"
      transform="rotate(45 4 5)"
    />
    <rect
      x="7.5"
      y="3.5"
      width="3"
      height="3"
      rx="0.75"
      stroke="currentColor"
      strokeWidth="1.1"
      transform="rotate(45 9 5)"
    />
    <rect
      x="12.5"
      y="3.5"
      width="3"
      height="3"
      rx="0.75"
      stroke="currentColor"
      strokeWidth="1.1"
      transform="rotate(45 14 5)"
    />
    <path
      d="M3.62 11.12L6.38 8.88M9.62 11.12L12.38 8.88"
      stroke="currentColor"
      strokeWidth="1.1"
      strokeLinecap="round"
    />
  </svg>
);

export const PinIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ width: 10, height: 10, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M8 1.5V7L10.5 9.5L6.5 10L8 14.5L9.5 10L5.5 9.5L8 7V1.5Z"
      fill="currentColor"
    />
  </svg>
);

export const LightbulbIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ width: 20, height: 20, flexShrink: 0, ...style }}
    viewBox="0 0 20 20"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M10 2.5C7.1 2.5 4.8 4.8 4.8 7.7C4.8 9.3 5.7 10.7 7.1 11.5L7 14H13L12.9 11.5C14.3 10.7 15.2 9.3 15.2 7.7C15.2 4.8 12.9 2.5 10 2.5Z"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinejoin="round"
    />
    <path
      d="M7.5 15.5H12.5"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
    <path
      d="M8 17.5H12"
      stroke="currentColor"
      strokeWidth="1.25"
      strokeLinecap="round"
    />
  </svg>
);

export const RocketIcon: FC<StageIconProps> = ({ className, style }) => (
  <svg
    className={className}
    style={{ width: 16, height: 16, flexShrink: 0, ...style }}
    viewBox="0 0 16 16"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M8 1.5C8 1.5 5.5 3.5 4 5.5C3 6.5 2.5 7.5 2.5 8.5C2.5 10 3.5 11.5 5 12.5C5.5 12.8 6 13 6.5 13C7 13 7.5 12.8 8 12.5C8.5 12.8 9 13 9.5 13C10 13 10.5 12.8 11 12.5C12.5 11.5 13.5 10 13.5 8.5C13.5 7.5 13 6.5 12 5.5C10.5 3.5 8 1.5 8 1.5Z"
      stroke="currentColor"
      strokeWidth="1.1"
      strokeLinejoin="round"
    />
    <circle cx="8" cy="8.5" r="1.5" stroke="currentColor" strokeWidth="1.1" />
    <path d="M5.5 12L3.5 14.5" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
    <path d="M10.5 12L12.5 14.5" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
  </svg>
);
