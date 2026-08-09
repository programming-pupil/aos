// ── Message bubble — shared between Chat, AgentChat, and Nl2sql ──────────────────

import { memo, useState, useCallback, useMemo } from "react";
import {
  Typography,
  Button,
  Tooltip,
  message as messageApi,
  Tag,
  Collapse,
  Empty,
} from "antd";
import type { CollapseProps } from "antd";
import {
  CopyOutlined,
  CheckOutlined,
  MessageOutlined,
  DownloadOutlined,
  Loading3QuartersOutlined,
  LoadingOutlined,
  RobotOutlined,
  UserOutlined,
  FileOutlined,
  LinkOutlined,
  DatabaseOutlined,
  SearchOutlined,
  HistoryOutlined,
  BugOutlined,
} from "@ant-design/icons";
import type {
  ChatMessage,
  ContentBlock,
  ChatEvidenceSource,
  ImageBlock,
  DocumentBlock,
  ToolCallInfo,
} from "./types";
import { Markdown } from "./markdownRenderer";
import { ThinkingBubble } from "./ThinkingBubble";
import { useTranslation } from "react-i18next";
import type { PmSearchUsageSummary } from "./chatCore.pmTypes";
import { AuthenticatedUploadImage } from "./AuthenticatedUploadImage";

const { Text } = Typography;

type AssistantVariant = "chat" | "pm" | "agent";

interface MessageInsightFooterProps {
  sources: ChatEvidenceSource[];
  toolCalls: ToolCallInfo[];
  searchUsage?: PmSearchUsageSummary;
  traceEvents?: Record<string, unknown>[];
  t: ReturnType<typeof useTranslation>["t"];
}

interface MessageBubbleProps {
  message: ChatMessage;
  isStreaming?: boolean;
  modelName?: string;
  onReply?: (msgId: string) => void;
  /** Extra action buttons rendered in the action row (copy/download/reply). */
  extraActions?: React.ReactNode;
  /** Optional extra panel attached to this bubble (e.g. PM claim-evidence alignment). */
  extraPanel?: React.ReactNode;
  /** Optional placeholder text while streaming but no visible tokens/events yet. */
  streamingPlaceholderText?: string;
  /** Whether this message is the live streaming bubble. */
  isStreamingBubble?: boolean;
  thinkingExpanded?: boolean;
  onThinkingToggle?: () => void;
  variant?: AssistantVariant;
  traceEvents?: Record<string, unknown>[];
}

function renderContent(content: string | ContentBlock[], relaxed = false): React.ReactNode {
  if (typeof content === "string") {
    return <Markdown relaxed={relaxed}>{content}</Markdown>;
  }
  if (!Array.isArray(content)) {
    return <Markdown relaxed={relaxed}>{String(content)}</Markdown>;
  }
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {content.map((block, i) => {
        if (block.type === "text") {
          return <Markdown key={i} relaxed={relaxed}>{block.text}</Markdown>;
        }
        if (block.type === "image") {
          const img = block as ImageBlock;
          const src =
            img.sourceType === "url"
              ? (img.previewUrl ?? img.data)
              : `data:${img.media_type};base64,${img.data}`;
          return (
            <AuthenticatedUploadImage
              key={i}
              src={src}
              alt={img.name ?? "uploaded image"}
              style={{ maxWidth: 360, borderRadius: 8, cursor: "pointer" }}
              preview={{ mask: <span style={{ fontSize: 12 }}>🔍</span> }}
            />
          );
        }
        if (block.type === "document") {
          const doc = block as DocumentBlock;
          return (
            <div
              key={i}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "6px 12px",
                background: "var(--bg-interactive)",
                borderRadius: 6,
                fontSize: 13,
              }}
            >
              <FileOutlined style={{ color: "var(--text-secondary)" }} />
              <span style={{ fontWeight: 500 }}>{doc.name ?? "Document"}</span>
              <span style={{ color: "var(--text-muted)", fontSize: 11 }}>
                {doc.media_type}
              </span>
            </div>
          );
        }
        return null;
      })}
    </div>
  );
}

function cleanupPmVisibleContent(content: string): string {
  return content
    .replace(/\bDetected first-party evidence:\s*\d+\s+metric signals?\s+and\s+\d+\s+opportunity cohorts?\.?/gi, " ")
    .replace(/^\s*(?:一手片段|First-party snippets):.*$/gim, "")
    .replace(/(?:^|\s)[+＋]\s*\d+\s+more\.?/gi, " ")
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function renderPlainContent(content: string | ContentBlock[]): React.ReactNode {
  const textStyle: React.CSSProperties = {
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
    wordBreak: "break-word",
  };
  if (typeof content === "string") {
    return <div style={textStyle}>{content}</div>;
  }
  if (!Array.isArray(content)) {
    return <div style={textStyle}>{String(content)}</div>;
  }
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {content.map((block, i) => {
        if (block.type === "text") {
          return (
            <div key={i} style={textStyle}>
              {block.text}
            </div>
          );
        }
        if (block.type === "image") {
          const img = block as ImageBlock;
          const src =
            img.sourceType === "url"
              ? (img.previewUrl ?? img.data)
              : `data:${img.media_type};base64,${img.data}`;
          return (
            <AuthenticatedUploadImage
              key={i}
              src={src}
              alt={img.name ?? "uploaded image"}
              style={{ maxWidth: 360, borderRadius: 8, cursor: "pointer" }}
              preview={{ mask: <span style={{ fontSize: 12 }}>🔍</span> }}
            />
          );
        }
        if (block.type === "document") {
          const doc = block as DocumentBlock;
          return (
            <div
              key={i}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "6px 12px",
                background: "var(--bg-interactive)",
                borderRadius: 6,
                fontSize: 13,
              }}
            >
              <FileOutlined style={{ color: "var(--text-secondary)" }} />
              <span style={{ fontWeight: 500 }}>{doc.name ?? "Document"}</span>
              <span style={{ color: "var(--text-muted)", fontSize: 11 }}>
                {doc.media_type}
              </span>
            </div>
          );
        }
        return null;
      })}
    </div>
  );
}

function messageContentToPlain(content: string | ContentBlock[]): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return String(content ?? "");
  return content
    .map((block) => {
      if (block.type === "text") return block.text;
      if (block.type === "image") return "[image]";
      if (block.type === "document") return `[document:${block.name ?? "Document"}]`;
      return "[block]";
    })
    .join("\n");
}

function stripHeadingNumberPrefix(text: string): string {
  return text
    .replace(/^\s*(?:#{1,6}\s*)?/, "")
    .replace(/^\s*(?:第?[一二三四五六七八九十百千零〇两]{1,6}[章节部分步]?|[0-9]{1,2})[、．.]\s*/, "")
    .replace(/^\s*(?:\(?[a-zA-Z]\)|[a-zA-Z][.)])\s+/, "")
    .trim();
}

function normalizeHeadingForMatch(text: string): string {
  return stripHeadingNumberPrefix(text)
    .replace(/\*\*/g, "")
    .replace(/[`_*~]/g, "")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase();
}

function extractMarkdownHeadings(content: string | ContentBlock[]): Array<{ level: number; text: string; displayText: string }> {
  const text = messageContentToPlain(content);
  const headings: Array<{ level: number; text: string; displayText: string }> = [];
  const inFence = { current: false };
  for (const line of text.split(/\r?\n/)) {
    if (line.trimStart().startsWith("```")) {
      inFence.current = !inFence.current;
      continue;
    }
    if (inFence.current) continue;
    const match = line.match(/^(#{2,4})\s+(.+?)\s*#*\s*$/);
    if (!match) continue;
    const headingText = match[2]
      .replace(/\*\*/g, "")
      .replace(/[`_*~]/g, "")
      .trim();
    if (!headingText || headingText.length > 80) continue;
    headings.push({
      level: match[1].length,
      text: headingText,
      displayText: stripHeadingNumberPrefix(headingText) || headingText,
    });
    if (headings.length >= 10) break;
  }
  return headings;
}

function hasVisibleMarkdownToc(content: string | ContentBlock[]): boolean {
  const text = messageContentToPlain(content);
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < Math.min(lines.length, 40); i += 1) {
    const normalized = lines[i]
      .replace(/^[#>\s*-]+/, "")
      .replace(/\*\*/g, "")
      .trim()
      .toLowerCase();
    if (!/^(目录|contents?|table of contents)[:：]?$/.test(normalized)) {
      continue;
    }
    const following = lines
      .slice(i + 1, i + 8)
      .filter((line) => /^\s*(?:[-*]\s+|\d+[.)、．]\s+|[一二三四五六七八九十]+[、．]\s+)/.test(line));
    if (following.length >= 2) return true;
  }
  return false;
}

function PmArticleToc({
  headings,
  t,
}: {
  headings: Array<{ level: number; text: string; displayText: string }>;
  t: ReturnType<typeof useTranslation>["t"];
}) {
  if (headings.length < 3) return null;
  const handleJump = (event: React.MouseEvent<HTMLButtonElement>, headingIndex: number) => {
    const container = event.currentTarget.closest(".aos-message-content--pm");
    if (!container) return;
    const renderedHeadings = Array.from(
      container.querySelectorAll<HTMLElement>(
        ".markdown-body h1, .markdown-body h2, .markdown-body h3, .markdown-body h4",
      ),
    );
    const targetMeta = headings[headingIndex];
    const targetText = normalizeHeadingForMatch(targetMeta.text);
    const sameTextBefore = headings
      .slice(0, headingIndex)
      .filter((item) => normalizeHeadingForMatch(item.text) === targetText)
      .length;
    let seen = 0;
    const target = renderedHeadings.find((node) => {
      if (normalizeHeadingForMatch(node.textContent ?? "") !== targetText) return false;
      if (seen === sameTextBefore) return true;
      seen += 1;
      return false;
    }) ?? renderedHeadings[headingIndex];
    target?.scrollIntoView({ behavior: "smooth", block: "start" });
  };
  return (
    <div className="aos-pm-article-toc">
      <div className="aos-pm-article-toc__title">
        {t("chat.pmArticleToc", "目录")}
      </div>
      <div className="aos-pm-article-toc__items">
        {headings.map((heading, index) => (
          <button
            type="button"
            key={`${heading.level}-${heading.text}-${index}`}
            className="aos-pm-article-toc__item"
            style={{ paddingLeft: Math.max(0, heading.level - 2) * 10 }}
            onClick={(event) => handleJump(event, index)}
          >
            <span className="aos-pm-article-toc__index">{index + 1}</span>
            <span>{heading.displayText}</span>
          </button>
        ))}
      </div>
    </div>
  );
}

function shortText(input: string, max = 120): string {
  const trimmed = input.trim().replace(/\s+/g, " ");
  if (trimmed.length <= max) return trimmed;
  return `${trimmed.slice(0, max - 1)}...`;
}

function coerceTimestampMs(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value > 0 && value < 10_000_000_000 ? value * 1000 : value;
  }
  if (typeof value === "string" && value.trim()) {
    const numeric = Number(value);
    if (Number.isFinite(numeric)) {
      return numeric > 0 && numeric < 10_000_000_000 ? numeric * 1000 : numeric;
    }
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function formatMessageTimestamp(
  message: ChatMessage & {
    created_at?: unknown;
    createdAt?: unknown;
    created_at_ms?: unknown;
    createdAtMs?: unknown;
    timestampMs?: unknown;
  },
): string | null {
  const timestamp =
    coerceTimestampMs(message.timestamp) ??
    coerceTimestampMs(message.createdAt) ??
    coerceTimestampMs(message.created_at) ??
    coerceTimestampMs(message.createdAtMs) ??
    coerceTimestampMs(message.created_at_ms) ??
    coerceTimestampMs(message.timestampMs);
  if (timestamp == null) return null;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return null;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

function tryParseLooseJson(raw: string): unknown | null {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    // continue
  }

  const objectMatches = raw.match(/\{[\s\S]*\}/g);
  if (objectMatches && objectMatches.length > 0) {
    for (let i = objectMatches.length - 1; i >= 0; i -= 1) {
      try {
        return JSON.parse(objectMatches[i]);
      } catch {
        // continue
      }
    }
  }

  const arrayMatches = raw.match(/\[[\s\S]*\]/g);
  if (arrayMatches && arrayMatches.length > 0) {
    for (let i = arrayMatches.length - 1; i >= 0; i -= 1) {
      try {
        return JSON.parse(arrayMatches[i]);
      } catch {
        // continue
      }
    }
  }

  return null;
}

function scalarToText(value: unknown): string | null {
  if (typeof value === "string") return shortText(value, 140);
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  if (Array.isArray(value)) {
    const head = value
      .slice(0, 3)
      .map((item) => (typeof item === "string" ? item : ""))
      .filter(Boolean)
      .join(" / ");
    return head ? shortText(head, 140) : null;
  }
  return null;
}

function findByKeys(node: unknown, keys: string[], depth = 0): string | null {
  if (depth > 4 || node == null) return null;
  const direct = scalarToText(node);
  if (typeof node !== "object") return direct;

  const normalized = new Set(keys.map((k) => k.toLowerCase()));
  if (Array.isArray(node)) {
    for (const item of node) {
      const found = findByKeys(item, keys, depth + 1);
      if (found) return found;
    }
    return null;
  }

  const entries = Object.entries(node as Record<string, unknown>);
  for (const [key, value] of entries) {
    const k = key.toLowerCase();
    if (normalized.has(k)) {
      const text = scalarToText(value);
      if (text) return text;
    }
  }
  for (const [, value] of entries) {
    const found = findByKeys(value, keys, depth + 1);
    if (found) return found;
  }
  return null;
}

function countFromKeys(node: unknown, keys: string[]): number | null {
  if (!node || typeof node !== "object") return null;
  if (Array.isArray(node)) return node.length;
  const entries = Object.entries(node as Record<string, unknown>);
  const keySet = new Set(keys.map((k) => k.toLowerCase()));
  for (const [key, value] of entries) {
    if (!keySet.has(key.toLowerCase())) continue;
    if (Array.isArray(value)) return value.length;
    if (typeof value === "number") return value;
  }
  return null;
}

function normalizeToolName(raw: string): string {
  return raw
    .replace(/_/g, " ")
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .replace(/\s+/g, " ")
    .trim();
}

type ToolStage =
  | "search"
  | "extract"
  | "verify"
  | "analyze"
  | "synthesize"
  | "execute";

function inferToolStage(name: string): ToolStage {
  const n = name.toLowerCase();
  if (/(web|search|query|lookup|find)/.test(n)) return "search";
  if (/(extract|scrape|crawl|fetch|visit|open|read|download|browser)/.test(n))
    return "extract";
  if (/(verify|validate|fact|check|ground|evidence)/.test(n)) return "verify";
  if (/(cluster|rank|score|classify|analy|analysis)/.test(n)) return "analyze";
  if (/(summar|synth|compose|write|draft|report|answer)/.test(n))
    return "synthesize";
  return "execute";
}

function stageMeta(
  stage: ToolStage,
  t?: ReturnType<typeof useTranslation>["t"],
): { label: string; color: string } {
  switch (stage) {
    case "search":
      return { label: t ? t("chat.toolStageSearch") : "检索", color: "blue" };
    case "extract":
      return { label: t ? t("chat.toolStageExtract") : "提取", color: "cyan" };
    case "verify":
      return { label: t ? t("chat.toolStageVerify") : "校验", color: "gold" };
    case "analyze":
      return {
        label: t ? t("chat.toolStageAnalyze") : "分析",
        color: "purple",
      };
    case "synthesize":
      return {
        label: t ? t("chat.toolStageSynthesize") : "生成",
        color: "green",
      };
    default:
      return {
        label: t ? t("chat.toolStageExecute") : "执行",
        color: "default",
      };
  }
}

function extractTarget(argsRaw: string): string | null {
  const normalizeCandidate = (input: string | null): string | null => {
    if (!input) return null;
    const compact = input.replace(/\s+/g, " ").trim();
    if (!compact) return null;
    const commandLike =
      compact.length > 140 ||
      /(python3?\s+-|<<'PY|import\s+[a-zA-Z_][\w.]*|def\s+[a-zA-Z_]\w*\(|curl\s+https?:\/\/|bash\s+-c|node\s+-e|SELECT\s+.+\s+FROM\s+)/i.test(
        compact,
      );
    if (commandLike) return null;
    return shortText(compact, 120);
  };

  const parsed = tryParseLooseJson(argsRaw);
  const fromJsonRaw = findByKeys(parsed, [
    "query",
    "q",
    "keyword",
    "keywords",
    "question",
    "prompt",
    "topic",
    "term",
    "url",
    "urls",
    "uri",
    "site",
    "domain",
    "target",
  ]);
  const fromJson = normalizeCandidate(fromJsonRaw);
  if (fromJson) return fromJson;

  const queryMatch =
    argsRaw.match(/"query"\s*:\s*"([^"]+)"/i) ??
    argsRaw.match(/"q"\s*:\s*"([^"]+)"/i);
  const query = normalizeCandidate(queryMatch?.[1] ?? null);
  if (query) return query;

  const urlMatch = argsRaw.match(/https?:\/\/[^\s"']+/i);
  if (urlMatch?.[0]) return shortText(urlMatch[0], 120);
  return null;
}

function summarizeResult(resultRaw: string): string | null {
  const parsed = tryParseLooseJson(resultRaw);
  const count = countFromKeys(parsed, [
    "results",
    "items",
    "sources",
    "documents",
    "hits",
    "rows",
  ]);
  if (count != null) return `#${count}`;

  const text = findByKeys(parsed, [
    "summary",
    "snippet",
    "content",
    "text",
    "message",
    "error",
    "reason",
  ]);
  if (text) return shortText(text, 110);

  if (resultRaw && resultRaw.trim()) return shortText(resultRaw, 110);
  return null;
}

function extractUrlsFromText(text: string): string[] {
  if (!text) return [];
  const matches = text.match(/https?:\/\/[^\s)\]]+/g) ?? [];
  const uniq = new Set<string>();
  for (const raw of matches) {
    uniq.add(raw.replace(/[.,;:!?]+$/, ""));
  }
  return Array.from(uniq);
}

function extractDomainsFromTools(toolCalls: ToolCallInfo[]): string[] {
  const domains = new Set<string>();
  for (const tool of toolCalls) {
    const text = `${tool.args ?? ""}\n${tool.result ?? ""}`;
    for (const url of extractUrlsFromText(text)) {
      try {
        const host = new URL(url).host;
        if (host) domains.add(host.toLowerCase());
      } catch {
        // ignore invalid URL
      }
    }
  }
  return Array.from(domains);
}

function stripEmptySourceTail(text: string): string {
  return text.replace(
    /(?:\r?\n){0,2}(?:来源|参考来源|Sources?|References?)\s*[:：]\s*$/i,
    "",
  );
}

function mergeEvidenceSources(
  explicitSources: ChatEvidenceSource[] | undefined,
  content: string | ContentBlock[],
  toolCalls: ToolCallInfo[] | undefined,
): ChatEvidenceSource[] {
  const out: ChatEvidenceSource[] = [...(explicitSources ?? [])];
  const seen = new Set(out.map((source) => source.url || source.fileId || source.id));
  const contentText = typeof content === "string" ? content : messageContentToPlain(content);
  const combined = [
    contentText,
    ...(toolCalls ?? []).map((tool) => `${tool.args ?? ""}\n${tool.result ?? ""}`),
  ].join("\n");
  for (const rawUrl of extractUrlsFromText(combined)) {
    const key = rawUrl;
    if (seen.has(key)) continue;
    seen.add(key);
    let title = rawUrl;
    try {
      title = new URL(rawUrl).hostname || rawUrl;
    } catch {
      // keep raw URL
    }
    out.push({
      id: `web-${out.length}-${rawUrl}`,
      type: "web",
      title,
      url: rawUrl,
    });
  }
  return out.slice(0, 12);
}

function buildToolNarrative(
  tool: ToolCallInfo,
  t?: ReturnType<typeof useTranslation>["t"],
): {
  stageLabel: string;
  stageColor: string;
  headline: string;
  detail: string | null;
  resultPreview: string | null;
} {
  const stage = inferToolStage(tool.name);
  const stageInfo = stageMeta(stage, t);
  const target = extractTarget(tool.args);
  const resultPreview = summarizeResult(tool.result);

  const runningPrefix = t ? t("chat.toolStatusRunning") : "正在";
  const donePrefix = t ? t("chat.toolStatusDone") : "已";
  const failedPrefix = t ? t("chat.toolStatusFailed") : "失败";
  const joinWord = (prefix: string, verb: string) =>
    /[A-Za-z]$/.test(prefix) ? `${prefix} ${verb}` : `${prefix}${verb}`;
  const joinWordPost = (verb: string, suffix: string) =>
    /^[A-Za-z]/.test(suffix) ? `${verb} ${suffix}` : `${verb}${suffix}`;
  const actionVerb =
    stage === "search"
      ? t
        ? t("chat.toolVerbSearch")
        : "搜索"
      : stage === "extract"
        ? t
          ? t("chat.toolVerbExtract")
          : "提取"
        : stage === "verify"
          ? t
            ? t("chat.toolVerbVerify")
            : "校验"
          : stage === "analyze"
            ? t
              ? t("chat.toolVerbAnalyze")
              : "分析"
            : stage === "synthesize"
              ? t
                ? t("chat.toolVerbSynthesize")
                : "整理"
              : t
                ? t("chat.toolVerbExecute")
                : "执行";
  const fallbackTarget =
    stage === "execute"
      ? t
        ? t("chat.toolTargetInternalTask", "系统任务")
        : "系统任务"
      : normalizeToolName(tool.name);
  const targetText = target ? `「${target}」` : fallbackTarget;

  const headline =
    tool.status === "error"
      ? `${joinWordPost(actionVerb, failedPrefix)}: ${targetText}`
      : tool.status === "success"
        ? `${joinWord(donePrefix, actionVerb)}: ${targetText}`
        : `${joinWord(runningPrefix, actionVerb)}: ${targetText}`;

  const detail =
    target && targetText !== `「${target}」`
      ? target
      : target && targetText === `「${target}」`
        ? shortText(target, 120)
        : null;

  return {
    stageLabel: stageInfo.label,
    stageColor: stageInfo.color,
    headline,
    detail,
    resultPreview,
  };
}

function sourceOrigin(source: ChatEvidenceSource): string {
  if (source.type === "file") {
    const range =
      source.lineStart != null
        ? source.lineEnd != null && source.lineEnd !== source.lineStart
          ? `:${source.lineStart}-${source.lineEnd}`
          : `:${source.lineStart}`
        : "";
    return `${source.filename || source.title || "file"}${range}`;
  }
  if (source.type === "memory") {
    return source.sourceLabel || source.title || source.memoryId || "memory";
  }
  if (source.url) {
    try {
      return new URL(source.url).hostname.replace(/^www\./, "");
    } catch {
      return source.url;
    }
  }
  return source.sourceLabel || source.title || "source";
}

function sourceTitle(source: ChatEvidenceSource, index: number): string {
  const fallback = `${source.type}-${index + 1}`;
  return (
    source.title ||
    source.filename ||
    source.sourceLabel ||
    source.url ||
    source.memoryId ||
    fallback
  );
}

function sourceGroupLabel(
  type: ChatEvidenceSource["type"],
  t: ReturnType<typeof useTranslation>["t"],
): string {
  if (type === "file") return t("chat.footerFiles", "Files");
  if (type === "memory") return t("chat.footerMemory", "Memory");
  return t("chat.footerWeb", "Web");
}

function sourceTypeColor(type: ChatEvidenceSource["type"]): string {
  if (type === "file") return "geekblue";
  if (type === "memory") return "magenta";
  return "cyan";
}

function dedupeSources(sources: ChatEvidenceSource[]): ChatEvidenceSource[] {
  const seen = new Set<string>();
  const out: ChatEvidenceSource[] = [];
  for (const source of sources) {
    const key = [
      source.type,
      source.url ?? "",
      source.fileId ?? "",
      source.memoryId ?? "",
      source.title ?? "",
      source.snippet ?? "",
    ].join("\u0001");
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(source);
    if (out.length >= 18) break;
  }
  return out;
}

function SourceCard({
  source,
  index,
  t,
}: {
  source: ChatEvidenceSource;
  index: number;
  t: ReturnType<typeof useTranslation>["t"];
}) {
  const title = sourceTitle(source, index);
  const origin = sourceOrigin(source);
  const card = (
    <div className="aos-message-source-card">
      <div className="aos-message-source-card__top">
        <Tag color={sourceTypeColor(source.type)} style={{ marginRight: 0 }}>
          [{index + 1}]
        </Tag>
        <Text className="aos-message-source-card__title">{title}</Text>
      </div>
      <div className="aos-message-source-card__meta">
        <span>{sourceGroupLabel(source.type, t)}</span>
        <span>·</span>
        <span>{origin}</span>
      </div>
      {source.snippet && (
        <div className="aos-message-source-card__snippet">
          {shortText(source.snippet, 220)}
        </div>
      )}
    </div>
  );
  if (source.url) {
    return (
      <a href={source.url} target="_blank" rel="noreferrer" className="aos-message-source-link">
        {card}
      </a>
    );
  }
  return card;
}

function SourcesSection({
  sources,
  t,
}: {
  sources: ChatEvidenceSource[];
  t: ReturnType<typeof useTranslation>["t"];
}) {
  const grouped = new Map<ChatEvidenceSource["type"], ChatEvidenceSource[]>();
  sources.forEach((source) => {
    const list = grouped.get(source.type) ?? [];
    list.push(source);
    grouped.set(source.type, list);
  });
  const order: ChatEvidenceSource["type"][] = ["web", "file", "memory"];
  if (sources.length === 0) {
    return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("chat.footerNoSources", "No sources for this reply")} />;
  }
  return (
    <div className="aos-message-footer-section">
      {order
        .filter((type) => (grouped.get(type)?.length ?? 0) > 0)
        .map((type) => (
          <div key={type} className="aos-message-source-group">
            <div className="aos-message-source-group__title">
              {sourceGroupLabel(type, t)} · {grouped.get(type)!.length}
            </div>
            <div className="aos-message-source-grid">
              {grouped.get(type)!.map((source, idx) => (
                <SourceCard
                  key={`${source.id}-${source.url ?? source.fileId ?? source.memoryId ?? idx}`}
                  source={source}
                  index={sources.indexOf(source)}
                  t={t}
                />
              ))}
            </div>
          </div>
        ))}
    </div>
  );
}

function buildSearchUsageRows(
  searchUsage: PmSearchUsageSummary | undefined,
  t: ReturnType<typeof useTranslation>["t"],
): Array<{
  key: string;
  label: string;
  text: string;
  tone: "ok" | "warn" | "muted";
}> {
  return (searchUsage?.rows ?? [])
    .filter((row) => row.attempts > 0 || row.successCount > 0 || row.errorCount > 0)
    .map((row) => {
      const ok = row.successCount > 0;
      const warn = row.errorCount > 0 && row.successCount === 0;
      const parts = [
        t("chat.footerSearchAttempts", {
          count: row.attempts,
          defaultValue: `调用 ${row.attempts} 次`,
        }),
        row.successCount > 0
          ? t("chat.footerSearchSuccess", {
              count: row.successCount,
              defaultValue: `成功 ${row.successCount}`,
            })
          : "",
        row.errorCount > 0
          ? t("chat.footerSearchFailed", {
              count: row.errorCount,
              defaultValue: `失败 ${row.errorCount}`,
            })
          : "",
        (row.skippedCount ?? 0) > 0
          ? t("chat.footerSearchSkipped", {
              count: row.skippedCount,
              defaultValue: `跳过 ${row.skippedCount}`,
            })
          : "",
        (row.resultCount ?? 0) > 0
          ? t("chat.footerSearchResults", {
              count: row.resultCount,
              defaultValue: `结果 ${row.resultCount}`,
            })
          : "",
      ].filter(Boolean);
      return {
        key: row.layer,
        label: row.label || row.layer,
        text: parts.join(" · "),
        tone: warn ? "warn" : ok ? "ok" : "muted",
      };
    });
}

function traceActivityRows(
  traceEvents: Record<string, unknown>[] | undefined,
  t: ReturnType<typeof useTranslation>["t"],
): Array<{ headline: string; detail?: string }> {
  const rows: Array<{ headline: string; detail?: string }> = [];
  for (const event of traceEvents ?? []) {
    const stage =
      typeof event.stage === "string"
        ? event.stage
        : typeof event.event === "string"
          ? event.event
          : typeof event.type === "string"
            ? event.type
            : typeof event.name === "string"
              ? event.name
              : "";
    const status =
      typeof event.status === "string"
        ? event.status
        : typeof event.action === "string"
          ? event.action
          : "";
    const detail =
      findByKeys(event, [
        "message",
        "summary",
        "reason",
        "query",
        "provider",
        "layer",
        "tool",
        "source",
      ]) ?? undefined;
    const headlineParts = [stage, status]
      .map((part) => part.trim())
      .filter(Boolean);
    const headline =
      headlineParts.length > 0
        ? headlineParts.join(" · ")
        : t("chat.footerTraceEvent", "Background event");
    rows.push({
      headline: shortText(headline, 96),
      detail: detail ? shortText(detail, 160) : undefined,
    });
  }
  return rows.slice(-8);
}

function ActivitySection({
  toolCalls,
  searchUsage,
  traceEvents,
  t,
}: {
  toolCalls: ToolCallInfo[];
  searchUsage?: PmSearchUsageSummary;
  traceEvents?: Record<string, unknown>[];
  t: ReturnType<typeof useTranslation>["t"];
}) {
  const searchRows = buildSearchUsageRows(searchUsage, t);
  const sortedTools = [...toolCalls].sort((a, b) => a.index - b.index).slice(0, 10);
  const toolRows = sortedTools.map((tool) => buildToolNarrative(tool, t));
  const traceRows =
    searchRows.length === 0 && toolRows.length === 0
      ? traceActivityRows(traceEvents, t)
      : [];
  if (searchRows.length === 0 && toolRows.length === 0 && traceRows.length === 0) {
    return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("chat.footerNoActivity", "No activity recorded")} />;
  }
  return (
    <div className="aos-message-footer-section">
      {searchRows.length > 0 && (
        <div className="aos-message-activity-block">
          <div className="aos-message-activity-title">
            <SearchOutlined /> {t("chat.footerSearchSummary", "Search calls")}
          </div>
          <div className="aos-message-search-usage-grid">
            {searchRows.map((row) => (
              <div key={row.key} className={`aos-message-search-usage is-${row.tone}`}>
                <span className="aos-message-search-usage__label">{row.label}</span>
                <span className="aos-message-search-usage__text">{row.text}</span>
              </div>
            ))}
          </div>
        </div>
      )}
      {toolRows.length > 0 && (
        <div className="aos-message-activity-block">
          <div className="aos-message-activity-title">
            <HistoryOutlined /> {t("chat.footerProcessSummary", "Process summary")}
          </div>
          <div className="aos-message-activity-list">
            {toolRows.map((row, idx) => (
              <div key={`${row.headline}-${idx}`} className="aos-message-activity-row">
                <Tag color={row.stageColor} style={{ marginRight: 0 }}>
                  {row.stageLabel}
                </Tag>
                <div>
                  <div className="aos-message-activity-row__headline">{row.headline}</div>
                  {row.resultPreview && (
                    <div className="aos-message-activity-row__detail">{row.resultPreview}</div>
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
      {toolCalls.length > sortedTools.length && (
        <Text style={{ fontSize: 12, color: "var(--text-muted)" }}>
          {t("chat.footerActivityMore", {
            count: toolCalls.length - sortedTools.length,
            defaultValue: `还有 ${toolCalls.length - sortedTools.length} 个步骤已收起在 Trace 中`,
          })}
        </Text>
      )}
      {traceRows.length > 0 && (
        <div className="aos-message-activity-block">
          <div className="aos-message-activity-title">
            <HistoryOutlined /> {t("chat.footerRecentActivity", "Recent activity")}
          </div>
          <div className="aos-message-activity-list">
            {traceRows.map((row, idx) => (
              <div key={`${row.headline}-${idx}`} className="aos-message-activity-row">
                <Tag color="default" style={{ marginRight: 0 }}>
                  {idx + 1}
                </Tag>
                <div>
                  <div className="aos-message-activity-row__headline">{row.headline}</div>
                  {row.detail && (
                    <div className="aos-message-activity-row__detail">{row.detail}</div>
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function TraceSection({
  toolCalls,
  searchUsage,
  traceEvents,
  t,
}: {
  toolCalls: ToolCallInfo[];
  searchUsage?: PmSearchUsageSummary;
  traceEvents?: Record<string, unknown>[];
  t: ReturnType<typeof useTranslation>["t"];
}) {
  const [expandedRows, setExpandedRows] = useState<Record<number, boolean>>({});
  const hasTrace =
    toolCalls.length > 0 ||
    (searchUsage?.rows?.length ?? 0) > 0 ||
    (traceEvents?.length ?? 0) > 0;
  if (!hasTrace) {
    return <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("chat.footerNoTrace", "No trace details")} />;
  }
  return (
    <div className="aos-message-footer-section">
      {toolCalls.length > 0 && (
        <div>
          <div className="aos-message-activity-title">
            <BugOutlined /> {t("chat.footerToolDetails", "Tool details")}
          </div>
          {toolCalls.map((tc, idx) => (
            <ToolCallCardInline
              key={`${tc.index}-${tc.name}-${idx}`}
              tool={tc}
              expanded={!!expandedRows[idx]}
              onToggle={() =>
                setExpandedRows((prev) => ({ ...prev, [idx]: !prev[idx] }))
              }
              t={t}
            />
          ))}
        </div>
      )}
      {(searchUsage?.rows?.length ?? 0) > 0 && (
        <pre className="aos-message-trace-pre">
          {JSON.stringify({ searchUsage }, null, 2)}
        </pre>
      )}
      {(traceEvents?.length ?? 0) > 0 && (
        <pre className="aos-message-trace-pre">
          {JSON.stringify({ trace: traceEvents?.slice(-20) }, null, 2)}
        </pre>
      )}
    </div>
  );
}

// Tool call card (inlined to avoid circular imports)
function ToolCallCardInline({
  tool,
  expanded,
  onToggle,
  t,
}: {
  tool: ToolCallInfo;
  expanded: boolean;
  onToggle?: () => void;
  t?: ReturnType<typeof useTranslation>["t"];
}) {
  const tryParse = (raw: string): string => {
    if (!raw) return "";
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      return raw;
    }
  };
  const parsedArgs = tryParse(tool.args);
  const parsedResult = tryParse(tool.result);
  const hasBody = !!(parsedArgs || parsedResult);
  const narrative = buildToolNarrative(tool, t);

  const borderColor = tool.isError
    ? "var(--color-error)"
    : "rgba(88,166,255,0.3)";
  const bgColor = tool.isError
    ? "rgba(255,77,79,0.04)"
    : "rgba(24,144,255,0.04)";
  const headerBg = tool.isError
    ? "rgba(255,77,79,0.08)"
    : "rgba(24,144,255,0.08)";

  return (
    <div
      style={{
        border: `1px solid ${borderColor}`,
        borderRadius: 8,
        marginTop: 6,
        overflow: "hidden",
        background: bgColor,
      }}
    >
      <div
        style={{
          display: "flex",
          gap: 8,
          padding: "8px 12px",
          background: headerBg,
          cursor: hasBody ? "pointer" : "default",
        }}
        onClick={hasBody ? onToggle : undefined}
      >
        <div style={{ marginTop: 2 }}>
          {tool.status === "success" ? (
            <span style={{ color: "var(--color-success)", fontSize: 13 }}>
              ✓
            </span>
          ) : tool.status === "error" ? (
            <span style={{ color: "var(--color-error)", fontSize: 13 }}>✕</span>
          ) : tool.status === "running" ? (
            <LoadingOutlined
              spin
              style={{ color: "var(--accent-ai)", fontSize: 13 }}
            />
          ) : (
            <span style={{ color: "var(--text-muted)", fontSize: 13 }}>○</span>
          )}
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              flexWrap: "wrap",
            }}
          >
            <Tag
              color={narrative.stageColor}
              style={{ fontSize: 10, marginRight: 0 }}
            >
              {narrative.stageLabel}
            </Tag>
            <Text
              style={{
                fontSize: 13,
                color: "var(--text-primary)",
                fontWeight: 600,
              }}
            >
              {narrative.headline}
            </Text>
            {tool.source === "mcp" && tool.mcpServer && (
              <Tag color="purple" style={{ fontSize: 10, marginRight: 0 }}>
                MCP: {tool.mcpServer}
              </Tag>
            )}
            {tool.source === "builtin" && (
              <Tag color="blue" style={{ fontSize: 10, marginRight: 0 }}>
                builtin
              </Tag>
            )}
            {tool.source === "skill" && (
              <Tag color="gold" style={{ fontSize: 10, marginRight: 0 }}>
                Skill: {tool.skillName || "skill"}
              </Tag>
            )}
          </div>
          {narrative.detail && (
            <Text
              style={{
                display: "block",
                fontSize: 12,
                color: "var(--text-muted)",
                marginTop: 2,
              }}
            >
              {narrative.detail}
            </Text>
          )}
          {narrative.resultPreview &&
            tool.status !== "running" &&
            tool.status !== "pending" && (
              <Text
                style={{
                  display: "block",
                  fontSize: 12,
                  color: tool.isError
                    ? "var(--color-error)"
                    : "var(--text-secondary)",
                  marginTop: 2,
                }}
              >
                {narrative.resultPreview}
              </Text>
            )}
        </div>
        <div
          style={{
            marginLeft: "auto",
            display: "flex",
            alignItems: "center",
            gap: 6,
          }}
        >
          {tool.status === "pending" && (
            <Tag color="processing" style={{ fontSize: 10, marginRight: 0 }}>
              <LoadingOutlined spin /> {t ? t("chat.toolPending") : "pending"}
            </Tag>
          )}
          {(tool.status === "success" || tool.status === "error") &&
            tool.durationMs != null && (
              <Text style={{ fontSize: 11, color: "var(--text-secondary)" }}>
                {tool.durationMs}ms
              </Text>
            )}
          {hasBody && (
            <span style={{ fontSize: 10, color: "var(--text-muted)" }}>
              {expanded ? "▲" : "▼"}
            </span>
          )}
        </div>
      </div>

      {hasBody && expanded && (
        <div
          style={{ padding: "8px 12px", borderTop: `1px solid ${borderColor}` }}
        >
          {parsedArgs && (
            <div style={{ marginBottom: 8 }}>
              <Text
                style={{
                  fontSize: 10,
                  color: "var(--text-secondary)",
                  textTransform: "uppercase",
                  letterSpacing: "0.5px",
                  display: "block",
                  marginBottom: 4,
                }}
              >
                {t ? t("chat.toolArgs") : "Args"}
              </Text>
              <pre
                style={{
                  margin: 0,
                  fontSize: 12,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-all",
                  color: "var(--text-secondary)",
                  background: "var(--bg-interactive)",
                  padding: 8,
                  borderRadius: 4,
                  maxHeight: 200,
                  overflow: "auto",
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
                  color: "var(--text-secondary)",
                  textTransform: "uppercase",
                  letterSpacing: "0.5px",
                  display: "block",
                  marginBottom: 4,
                }}
              >
                {t ? t("chat.toolResult") : "Result"}
              </Text>
              <pre
                style={{
                  margin: 0,
                  fontSize: 12,
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-all",
                  color: tool.isError
                    ? "var(--color-error)"
                    : "var(--text-secondary)",
                  background: "var(--bg-interactive)",
                  padding: 8,
                  borderRadius: 4,
                  maxHeight: 300,
                  overflow: "auto",
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

function MessageInsightFooter({
  sources,
  toolCalls,
  searchUsage,
  traceEvents,
  t,
}: MessageInsightFooterProps) {
  const cleanSources = dedupeSources(sources);
  const sourceSources = cleanSources.filter((source) => source.type !== "memory");
  const memorySources = cleanSources.filter((source) => source.type === "memory");
  const hasSearchUsage = (searchUsage?.rows?.length ?? 0) > 0;
  const hasTraceEvents = (traceEvents?.length ?? 0) > 0;
  const hasActivity = toolCalls.length > 0 || hasSearchUsage || hasTraceEvents;
  const hasTrace = toolCalls.length > 0 || hasSearchUsage || hasTraceEvents;
  const items: NonNullable<CollapseProps["items"]> = [];
  if (sourceSources.length > 0) {
    items.push({
      key: "sources",
      label: (
        <span className="aos-message-footer-label">
          <LinkOutlined /> {t("chat.sources", "Sources")} · {sourceSources.length}
        </span>
      ),
      children: <SourcesSection sources={sourceSources} t={t} />,
    });
  }
  if (hasActivity) {
    items.push({
      key: "activity",
      label: (
        <span className="aos-message-footer-label">
          <HistoryOutlined /> {t("chat.activity", "Activity")}
        </span>
      ),
      children: (
        <ActivitySection
          toolCalls={toolCalls}
          searchUsage={searchUsage}
          traceEvents={traceEvents}
          t={t}
        />
      ),
    });
  }
  if (memorySources.length > 0) {
    items.push({
      key: "memory",
      label: (
        <span className="aos-message-footer-label">
          <DatabaseOutlined /> {t("chat.footerMemory", "Memory")} · {memorySources.length}
        </span>
      ),
      children: <SourcesSection sources={memorySources} t={t} />,
    });
  }
  if (hasTrace) {
    items.push({
      key: "trace",
      label: (
        <span className="aos-message-footer-label">
          <BugOutlined /> {t("chat.trace", "Trace")}
        </span>
      ),
      children: (
        <TraceSection
          toolCalls={toolCalls}
          searchUsage={searchUsage}
          traceEvents={traceEvents}
          t={t}
        />
      ),
    });
  }

  if (items.length === 0) return null;

  return (
    <div className="aos-message-footer">
      <Collapse
        ghost
        size="small"
        items={items}
        defaultActiveKey={[]}
        expandIconPosition="end"
      />
    </div>
  );
}

function MessageBubbleImpl({
  message,
  isStreaming,
  modelName,
  onReply,
  extraActions,
  extraPanel,
  streamingPlaceholderText,
  isStreamingBubble,
  thinkingExpanded,
  onThinkingToggle,
  variant = "chat",
  traceEvents,
}: MessageBubbleProps) {
  const [copied, setCopied] = useState(false);
  const { t } = useTranslation();
  const isUser = message.role === "user";
  const displayContent = useMemo(() => {
    if (typeof message.content !== "string") return message.content;
    const stripped = stripEmptySourceTail(message.content);
    return !isUser && variant === "pm" ? cleanupPmVisibleContent(stripped) : stripped;
  }, [isUser, message.content, variant]);
  const hasTextContent =
    !!displayContent &&
    (typeof displayContent === "string"
      ? displayContent.trim().length > 0
      : displayContent.length > 0);
  const hasThinking = !!message.thinking && message.thinking.trim().length > 0;
  const hasToolCalls = !!message.toolCalls && message.toolCalls.length > 0;
  const showBubble =
    isUser || hasTextContent || hasThinking || hasToolCalls || !!isStreaming;

  const handleCopy = useCallback(async () => {
    const text = messageContentToPlain(displayContent);
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      messageApi.error(t("chat.copyFailed") ?? "copy failed");
    }
  }, [displayContent, t]);

  const handleDownloadReply = useCallback(() => {
    try {
      const body = messageContentToPlain(displayContent).trim();
      const lines: string[] = [];
      lines.push("# Assistant Reply");
      lines.push("");
      lines.push(`- Message ID: ${message.id}`);
      lines.push(`- Exported At: ${new Date().toISOString()}`);
      lines.push("");
      if (message.thinking && message.thinking.trim().length > 0) {
        lines.push("## Thinking");
        lines.push("");
        lines.push(message.thinking.trim());
        lines.push("");
      }
      lines.push("## Content");
      lines.push("");
      lines.push(body || "_[empty]_");
      lines.push("");
      if (Array.isArray(message.toolCalls) && message.toolCalls.length > 0) {
        lines.push("## Tool Calls");
        lines.push("");
        message.toolCalls.forEach((tc, idx) => {
          lines.push(
            `${idx + 1}. ${tc.name} · ${tc.status}${tc.durationMs != null ? ` · ${tc.durationMs}ms` : ""}`,
          );
        });
        lines.push("");
      }

      const blob = new Blob([lines.join("\n")], {
        type: "text/markdown;charset=utf-8",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      const safeId = (message.id || "reply").replace(/[^a-zA-Z0-9_-]/g, "_");
      a.href = url;
      a.download = `assistant-reply-${safeId}.md`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      messageApi.success(t("chat.replyDownloadSuccess", "回复已下载"));
    } catch {
      messageApi.error(t("chat.replyDownloadFailed", "下载回复失败"));
    }
  }, [displayContent, message.id, message.thinking, message.toolCalls, t]);

  const showTools = message.toolCalls && message.toolCalls.length > 0;
  const messageMeta = message as ChatMessage & {
    pmSearchUsage?: PmSearchUsageSummary;
    traceEvents?: Record<string, unknown>[];
  };
  const evidenceSources = mergeEvidenceSources(
    message.evidenceSources,
    displayContent,
    message.toolCalls,
  );
  const pmArticleHeadings = useMemo(
    () =>
      !isUser && variant === "pm" && hasTextContent
        ? extractMarkdownHeadings(displayContent)
        : [],
    [displayContent, hasTextContent, isUser, variant],
  );
  const shouldShowPmArticleToc = useMemo(
    () =>
      !isUser &&
      variant === "pm" &&
      pmArticleHeadings.length >= 3 &&
      !hasVisibleMarkdownToc(displayContent),
    [displayContent, isUser, pmArticleHeadings.length, variant],
  );
  const showStreamingPlaceholder =
    !!isStreamingBubble && !hasTextContent && !hasThinking && !hasToolCalls;
  const showStreamingStageHint =
    !!isStreamingBubble &&
    !!streamingPlaceholderText &&
    !showStreamingPlaceholder;
  const messageTimestamp = formatMessageTimestamp(message as ChatMessage & {
    created_at?: unknown;
    createdAt?: unknown;
    created_at_ms?: unknown;
    createdAtMs?: unknown;
    timestampMs?: unknown;
  });
  const responseModelName = !isUser
    ? message.modelName?.trim() || modelName?.trim()
    : undefined;
  if (!showBubble) return null;

  return (
    <div style={{ display: "flex", gap: 12, alignItems: "flex-start" }}>
      {/* Avatar */}
      <div
        style={{
          width: 32,
          height: 32,
          borderRadius: "50%",
          background: isUser
            ? "var(--bubble-user-icon-bg)"
            : "var(--bubble-assistant-icon-bg)",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          color: "#fff",
          fontSize: 14,
          flexShrink: 0,
        }}
      >
        {isUser ? (
          <UserOutlined />
        ) : isStreaming ? (
          <Loading3QuartersOutlined spin style={{ fontSize: 14 }} />
        ) : (
          <RobotOutlined />
        )}
      </div>

      {/* Content */}
      <div
        style={{
          flex: 1,
          maxWidth: "min(92%, 920px)",
        }}
      >
        {/* Role label */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            marginBottom: 6,
            flexWrap: "wrap",
          }}
        >
          <Text strong style={{ fontSize: 12, color: "var(--text-secondary)" }}>
            {isUser
              ? (t("chat.you") ?? "you")
              : responseModelName || t("chat.assistant", "AI Assistant")}
          </Text>
          {!isUser && message.judgeModel && (
            <Tag color="blue" style={{ marginRight: 0, fontSize: 11 }}>
              {t("chat.adversarialJudgeWithModel", "裁判模型：{{model}}", {
                model: message.judgeModel,
              })}
            </Tag>
          )}
          {!isUser && !message.isStreaming && message.winnerModel && (
            <Tooltip title={message.winnerReason || undefined}>
              <Tag color="green" style={{ marginRight: 0, fontSize: 11 }}>
                {t("chat.adversarialWinnerWithModel", "胜出模型：{{model}}", {
                  model: message.winnerModel,
                })}
              </Tag>
            </Tooltip>
          )}
          {messageTimestamp && (
            <Text style={{ fontSize: 11, color: "var(--text-muted)", whiteSpace: "nowrap" }}>
              {messageTimestamp}
            </Text>
          )}
          {message.isBookmarked && (
            <span style={{ fontSize: 11, color: "#faad14" }}>★</span>
          )}
        </div>

        {/* Thinking bubble — non-streaming messages.
            The persisted assistant message carries its own thinking text
            and (when known) a `thinkingDurationMs`. We explicitly pass
            `loading={false}` so the bubble renders the "已深度思考 · Xs"
            done state rather than a spinner + "思考中…". This fixes the
            long-standing bug where historical messages kept displaying
            as if reasoning were still in progress. */}
        {!isStreamingBubble &&
          message.role === "assistant" &&
          message.thinking && (
            <ThinkingBubble
              text={message.thinking}
              expanded={!!thinkingExpanded}
              onToggle={onThinkingToggle}
              loading={false}
              durationMs={message.thinkingDurationMs}
              t={t}
            />
          )}

        {/* Thinking bubble — streaming. The live bubble follows the
            reporter's loading flag and falls back to the duration-rich
            "done" state at the moment `thinking_end` fires. */}
        {isStreamingBubble && message.thinking && (
          <ThinkingBubble
            text={message.thinking}
            expanded={!!thinkingExpanded}
            onToggle={onThinkingToggle}
            loading={message.thinkingLoading}
            durationMs={message.thinkingDurationMs}
            t={t}
          />
        )}

        {showStreamingPlaceholder && (
          <div
            style={{
              width: "100%",
              border: "1px solid var(--border-subtle)",
              borderRadius: 12,
              background: "var(--bg-elevated)",
              padding: "12px 14px",
              display: "flex",
              alignItems: "center",
              gap: 8,
              color: "var(--text-secondary)",
              fontSize: 13,
            }}
          >
            <Loading3QuartersOutlined
              spin
              style={{ color: "var(--accent-ai)" }}
            />
            <Text style={{ fontSize: 13, color: "var(--text-secondary)" }}>
              {streamingPlaceholderText ??
                t("chat.streamingPreparing", "正在思考并整理回答...")}
            </Text>
          </div>
        )}

        {/* Text content */}
        {(hasTextContent || isUser) && (
          <div
            className={
              !isUser && variant === "pm"
                ? "aos-message-content aos-message-content--pm"
                : "aos-message-content"
            }
            style={{
              background: isUser
                ? "var(--bubble-user-bg)"
                : "var(--bubble-assistant-bg)",
              borderRadius: 12,
              padding: isUser ? "12px 16px" : "18px 22px",
              wordBreak: "break-word",
              lineHeight: isUser ? 1.75 : 1.82,
              fontSize: isUser ? 14 : 15,
              border: "1px solid",
              borderColor: isUser
                ? "var(--bubble-user-border)"
                : "var(--bubble-assistant-border)",
              color: "var(--text-primary)",
              userSelect: "text",
              boxShadow: isUser ? undefined : "0 8px 24px rgba(15, 23, 42, 0.04)",
            }}
          >
            {isUser
              ? renderPlainContent(displayContent)
              : (
                  <>
                    {shouldShowPmArticleToc && (
                      <PmArticleToc headings={pmArticleHeadings} t={t} />
                    )}
                    {renderContent(displayContent, true)}
                  </>
                )}
            {isStreaming && (
              <span
                style={{
                  color: "var(--bubble-cursor)",
                  animation: "typing 1s step-end infinite",
                }}
              >
                ▍
              </span>
            )}
          </div>
        )}

        {showStreamingStageHint && (
          <div
            style={{
              width: "100%",
              border: "1px solid var(--border-subtle)",
              borderRadius: 10,
              background: "var(--bg-elevated)",
              padding: "6px 10px",
              display: "flex",
              alignItems: "center",
              gap: 8,
              marginTop: hasTextContent ? 8 : 0,
              marginBottom: showTools ? 8 : 0,
            }}
          >
            <Loading3QuartersOutlined
              spin
              style={{ color: "var(--accent-ai)", fontSize: 12 }}
            />
            <Text style={{ fontSize: 12, color: "var(--text-secondary)" }}>
              {streamingPlaceholderText}
            </Text>
          </div>
        )}

        {!isUser && (
          <MessageInsightFooter
            sources={evidenceSources}
            toolCalls={message.toolCalls ?? []}
            searchUsage={messageMeta.pmSearchUsage}
            traceEvents={traceEvents ?? messageMeta.traceEvents}
            t={t}
          />
        )}

        {extraPanel && <div style={{ marginTop: 8 }}>{extraPanel}</div>}

        {/* Actions — hover to reveal */}
        {!isStreaming && (
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 4,
              marginTop: 4,
              opacity: 0.6,
              transition: "opacity 0.2s",
            }}
            onMouseEnter={(e) => (e.currentTarget.style.opacity = "1")}
            onMouseLeave={(e) => (e.currentTarget.style.opacity = "0.6")}
          >
            <Tooltip title={t("chat.copy")}>
              <Button
                type="text"
                size="small"
                icon={copied ? <CheckOutlined /> : <CopyOutlined />}
                onClick={handleCopy}
                style={{
                  color: "var(--text-muted)",
                  padding: "2px 6px",
                  height: 24,
                }}
              />
            </Tooltip>
            {message.role === "assistant" && hasTextContent && (
              <Tooltip title={t("chat.downloadReply", "下载本条回复")}>
                <Button
                  type="text"
                  size="small"
                  icon={<DownloadOutlined />}
                  onClick={handleDownloadReply}
                  style={{
                    color: "var(--text-muted)",
                    padding: "2px 6px",
                    height: 24,
                  }}
                />
              </Tooltip>
            )}
            {extraActions}
            {onReply && (
              <Tooltip title={t("chat.reply")}>
                <Button
                  type="text"
                  size="small"
                  icon={<MessageOutlined />}
                  onClick={() => onReply(message.id)}
                  style={{
                    color: "var(--text-muted)",
                    padding: "2px 6px",
                    height: 24,
                  }}
                />
              </Tooltip>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

export const MessageBubble = memo(MessageBubbleImpl);
