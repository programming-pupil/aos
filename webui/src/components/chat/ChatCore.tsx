// ── ChatCore — shared chat engine for Chat.tsx and AgentChat.tsx ─────────────────────────────────────────
//
// Extracts all common chat logic into a single reusable component:
//   • Session CRUD (create, delete, rename, pin, bookmark)
//   • Session history loading
//   • File upload (drag & drop + click)
//   • Slash commands (filter, panel, Levenshtein ranking)
//   • SSE streaming (thinking, tool calls, text, usage)
//   • Message bubble rendering
//   • Input area (textarea, attachments, reply reference)
//   • Keyboard shortcuts (Enter, Shift+Enter, /, arrows)
//
// Consumer pages inject their own layout via render props and override only
// what differs between them (top bar, right panel, source type, etc.).
//
// Design principles:
//   1. Zero assumptions about layout — consumer provides layout via props.
//   2. All state stays inside ChatCore; pages are purely presentational.
//   3. Stream handlers are defined once; page only customises onStreamEnd.
//   4. No duplicate code — every shared pattern lives here.

import { useState, useRef, useCallback, useMemo, useEffect } from "react";
import { flushSync } from "react-dom";
import {
  Typography,
  Button,
  Space,
  Tag,
  Tooltip,
  message,
  Card,
  Segmented,
  Drawer,
  Input,
  List,
  Empty,
  Popconfirm,
} from "antd";
import {
  SendOutlined,
  PaperClipOutlined,
  Loading3QuartersOutlined,
  LoadingOutlined,
  DownloadOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  ProfileOutlined,
  ShareAltOutlined,
  CloseOutlined,
  GlobalOutlined,
  FileTextOutlined,
  DeleteOutlined,
  PauseCircleOutlined,
  PlayCircleOutlined,
  PlusOutlined,
} from "@ant-design/icons";
import {
  useInfiniteQuery,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import {
  agentApi,
  apiKeysApi,
  commandsApi,
  mcpApi,
  nl2sqlApi,
  skillsApi,
  streamAgentSession,
  streamSuperAssistantTurnEvents,
  streamChatAdversarialRunEvents,
  streamNl2sqlAttributionTask,
  type ChatArtifactEvidenceItem,
  type ChatTurnOptions,
  streamPmResearchTask,
  type AgentSessionStreamHandlers,
  type RuntimeApprovalPaused,
  type AgentManualCompactionResult,
  type AgentMemoryCitation,
  type PmTaskImageInput,
  type PmTaskDocumentInput,
  type PmSubtaskAttemptRow as ApiPmSubtaskAttemptRow,
  type PmResearchTaskEvent as ApiPmResearchTaskEvent,
  type PmSubtaskRuntimeRow as ApiPmSubtaskRuntimeRow,
  uploadFile,
  type ChatFileRecord,
} from "@/api";
import { queryKeys } from "@/api/queryKeys";
import { dataAttributionTaskBindingFromStage } from "@/api/superAssistantEventReducer";
import { useSystemEvents } from "@/api/systemEvents";
import type {
  ChatEvidenceSource,
  SlashCommandDef,
  ToolCallInfo,
  SessionItem,
} from "./types";
import type { ContentBlock, ImageBlock, DocumentBlock } from "@/types";
import type {
  AgentUsage,
  AttributionTaskEvent,
  ChatAdversarialRun,
  ChatAdversarialStreamEvent,
  SuperAssistantTurnMessageMetadata,
} from "@/types";
import { SessionList } from "./SessionList";
import { SlashCommandPanel } from "./SlashCommandPanel";
import { MessageBubble } from "./MessageBubble";
import { AdversarialAuditPanel } from "./AdversarialAuditPanel";
import { AttributionAuditPanel } from "./AttributionAuditPanel";
import {
  hasNl2sqlAuditToolCalls,
  isNl2sqlAuditTool,
  nl2sqlAuditToolCallsFromHistory,
  nl2sqlProgressEventsFromStageEvents,
  Nl2sqlAuditPanel,
} from "./Nl2sqlAuditPanel";
import {
  isChatAdversarialRunId,
  isNl2sqlAttributionTaskId,
  isPmResearchTaskId,
} from "./specialistTaskIds";
import { shouldShowLegacyPmQueue } from "./superAssistantExecutionUi";
import {
  IsolatedComposerTextarea,
  type IsolatedComposerTextareaHandle,
} from "./IsolatedComposerTextarea";
import { ToolCallCard } from "./ToolCallCard";
import { PmExecutionDrawer } from "./PmExecutionDrawer";
import { useAuthenticatedUploadUrl } from "./AuthenticatedUploadImage";
import {
  parseWebSlashCommand,
  resolveEffectiveModel,
  WEB_BUILTIN_SLASH_COMMANDS,
} from "./webBuiltinCommands";

const CHAT_SUPPORTED_DOCUMENT_UPLOAD_EXTENSIONS = new Set([
  ".txt",
  ".md",
  ".markdown",
  ".csv",
  ".json",
  ".jsonl",
  ".sql",
  ".html",
  ".htm",
  ".css",
  ".js",
  ".ts",
  ".tsx",
  ".jsx",
  ".xml",
  ".log",
  ".rtf",
  ".docx",
  ".xlsx",
]);
function slashListMarkdown(
  title: string,
  items: string[],
  emptyText: string,
): string {
  if (items.length === 0) return emptyText;
  return [`## ${title}`, "", ...items.map((item) => `- ${item}`)].join("\n");
}

function isUnsupportedDocumentUpload(file: File): boolean {
  const name = file.name.toLowerCase();
  if (file.type.startsWith("image/")) return false;
  return ![...CHAT_SUPPORTED_DOCUMENT_UPLOAD_EXTENSIONS].some((ext) =>
    name.endsWith(ext),
  );
}

async function registerUploadedDocumentsForSession(
  docs: DocumentBlock[],
  sessionId: string,
): Promise<Record<string, ChatFileRecord>> {
  const indexed: Record<string, ChatFileRecord> = {};
  for (const doc of docs) {
    if (!doc.fileId || doc.sourceType !== "url" || !doc.data) continue;
    const record = await agentApi.registerChatFile({
      fileId: doc.fileId,
      filename: doc.name ?? doc.fileId,
      mediaType: doc.media_type,
      size: doc.sizeBytes,
      url: doc.data,
      sessionId,
    });
    indexed[record.fileId] = record;
  }
  return indexed;
}
import {
  PmFinalDeliveryPanel,
  shouldShowPmFinalDelivery,
} from "./PmFinalDeliveryPanel";
import { extractMarkdownTableBlocks } from "./markdownRenderer";
import { PmInlineEvidencePanel } from "./PmInlineEvidencePanel";
import { PmInlineNarrativePanel } from "./PmInlineNarrativePanel";
import {
  countDistinctUsableChatModels,
  isSuperAdversarialNeedsModelsError,
  parseSuperAssistantSlashCommand,
  superAssistantSlashRequestOptions,
} from "./superAssistantSlashCommands";
import { PmTaskQueuePanel } from "./PmTaskQueuePanel";
import {
  approvalResumeHandlers,
  type SessionStreamHandlers,
} from "./approvalResume";
import { useTranslation } from "react-i18next";
import type {
  ChatCoreProps,
  DisplayMessage,
  StreamingHandlers,
} from "./chatCore.types";
export type {
  ChatCoreProps,
  DisplayMessage,
  StreamingHandlers,
} from "./chatCore.types";
import {
  PM_MAX_IMAGE_ATTACHMENTS,
  collectPmTaskAttachments,
  collectStreamDocuments,
  collectStreamImages,
  parsePersistedHistoryContent,
} from "./chatCore.attachments";
import {
  buildPmSharePreviewPayload,
  buildPmSharePreviewUrl,
} from "./chatCore.pmShare";
import {
  buildMessageContent,
  buildReplyPrefix,
  contentToPlain,
  filterSlashCommands,
  mergeToolInput,
  parseToolName,
} from "./chatCore.utils";
import {
  pastedTextFileName,
  pastedTextLooksLikeSql,
  shouldAttachPastedText,
} from "./chatCore.paste";
import { reconcilePmHistoryTerminalAssistant } from "./pmTerminalReconciler";
import type {
  PmClaimEvidence,
  PmConflictGraph,
  PmConflictRow,
  PmEvidenceTreeNode,
  PmFinalDeliveryArtifact,
  PmInlineAction,
  PmInlineSegment,
  PmLiveToolEvent,
  PmQualitySnapshot,
  PmQueuedPrompt,
  PmQueuedPromptDraft,
  PmReportArtifact,
  PmSearchUsageSummary,
  PmStageEvent,
  PmStageId,
  PmStageState,
  PmStageStatus,
  PmStrategyLeaderboardRow,
  PmStrategyRunRecord,
  PmToolSummary,
  PmToolSummarySample,
} from "./chatCore.pmTypes";
import {
  PM_PIPELINE_BUDGET_MS,
  PM_STAGE_BUDGET_MS,
  PM_STAGE_ORDER,
  shouldShowPmPostStreamNotice,
} from "./chatCore.pmTypes";

async function registerHistoricalImagesForSession(
  messages: DisplayMessage[],
  sessionId: string,
): Promise<Record<string, ChatFileRecord>> {
  const images = new Map<string, ImageBlock>();
  for (const message of messages) {
    if (!Array.isArray(message.content)) continue;
    for (const block of message.content) {
      if (
        block.type === "image" &&
        block.fileId &&
        block.sourceType === "url" &&
        block.data.startsWith("/api/v1/uploads/")
      ) {
        images.set(block.fileId, block);
      }
    }
  }

  const records: Record<string, ChatFileRecord> = {};
  const settled = await Promise.allSettled(
    [...images.values()].map((image) =>
      agentApi.registerChatFile({
        fileId: image.fileId!,
        filename: image.name ?? image.fileId!,
        mediaType: image.media_type,
        size: image.sizeBytes,
        url: image.data,
        sessionId,
      }),
    ),
  );
  for (const result of settled) {
    if (result.status === "fulfilled")
      records[result.value.fileId] = result.value;
  }
  return records;
}

function mergePmToolSummaries(
  prev: PmToolSummary | null | undefined,
  incoming: PmToolSummary | null | undefined,
): PmToolSummary | null {
  if (!prev && !incoming) return null;
  if (!prev) return incoming ?? null;
  if (!incoming) return prev;

  const byNameMap = new Map<string, { count: number; errorCount: number }>();
  const addByName = (
    rows: Array<{ name: string; count: number; errorCount: number }>,
  ) => {
    for (const row of rows) {
      const current = byNameMap.get(row.name) ?? { count: 0, errorCount: 0 };
      current.count += row.count;
      current.errorCount += row.errorCount;
      byNameMap.set(row.name, current);
    }
  };
  addByName(prev.byName);
  addByName(incoming.byName);

  const samples: PmToolSummarySample[] = [];
  const seenSample = new Set<string>();
  for (const sample of [...prev.samples, ...incoming.samples]) {
    const key = `${sample.idx}|${sample.tool}|${sample.input ?? ""}|${sample.output ?? ""}|${sample.isError ? 1 : 0}`;
    if (seenSample.has(key)) continue;
    seenSample.add(key);
    samples.push(sample);
    if (samples.length >= 24) break;
  }

  const byName = Array.from(byNameMap.entries())
    .map(([name, value]) => ({ name, ...value }))
    .sort((a, b) => b.count - a.count)
    .slice(0, 12);

  return {
    count: prev.count + incoming.count,
    errorCount: prev.errorCount + incoming.errorCount,
    byName,
    samples,
  };
}

const PM_SEARCH_LAYER_LABELS: Record<string, string> = {
  multi_source: "多源组合",
  native_model_search: "模型原生",
  mcp_search: "MCP",
  configured_search_provider: "Search 扩展",
  rag_local: "RAG/local",
  local_evidence: "本地证据",
  web_search_tool: "WebSearch",
  web_fetch_tool: "WebFetch",
};

function normalizeSearchLayerName(input: string): string {
  const lower = input.trim().toLowerCase();
  if (!lower) return "";
  if (
    lower.includes("native_model_search") ||
    lower.includes("native web") ||
    lower.includes("native search")
  ) {
    return "native_model_search";
  }
  if (lower.includes("multi_source") || lower.includes("multi source"))
    return "multi_source";
  if (lower.includes("mcp")) return "mcp_search";
  if (
    lower.includes("configured_search_provider") ||
    lower.includes("search provider") ||
    lower.includes("brave") ||
    lower.includes("tavily") ||
    lower.includes("serper") ||
    lower.includes("exa") ||
    lower.includes("searx")
  ) {
    return "configured_search_provider";
  }
  if (lower.includes("rag") || lower.includes("local")) return "rag_local";
  return lower.replace(/[^a-z0-9_]+/g, "_");
}

function parsePmSearchUsageSummary(
  detail?: Record<string, unknown>,
  toolSummary?: PmToolSummary | null,
): PmSearchUsageSummary | null {
  const rows = new Map<string, PmSearchUsageSummary["rows"][number]>();
  const ensure = (layerRaw: string) => {
    const layer = normalizeSearchLayerName(layerRaw);
    if (!layer) return null;
    const current = rows.get(layer) ?? {
      layer,
      label: PM_SEARCH_LAYER_LABELS[layer] ?? layer,
      attempts: 0,
      successCount: 0,
      errorCount: 0,
      skippedCount: 0,
      resultCount: 0,
    };
    rows.set(layer, current);
    return current;
  };
  const addTrace = (trace: Record<string, unknown>) => {
    const layerRaw =
      typeof trace.layer === "string"
        ? trace.layer
        : typeof trace.sourceType === "string"
          ? trace.sourceType
          : "";
    const row = ensure(layerRaw);
    if (!row) return;
    const attempts =
      typeof trace.attempts === "number" && Number.isFinite(trace.attempts)
        ? Math.max(0, Math.round(trace.attempts))
        : 1;
    row.attempts += attempts;
    const explicitSuccess =
      typeof trace.successCount === "number" &&
      Number.isFinite(trace.successCount)
        ? Math.max(0, Math.round(trace.successCount))
        : null;
    const explicitErrors =
      typeof trace.errorCount === "number" && Number.isFinite(trace.errorCount)
        ? Math.max(0, Math.round(trace.errorCount))
        : null;
    const explicitSkipped =
      typeof trace.skippedCount === "number" &&
      Number.isFinite(trace.skippedCount)
        ? Math.max(0, Math.round(trace.skippedCount))
        : null;
    if (
      explicitSuccess != null ||
      explicitErrors != null ||
      explicitSkipped != null
    ) {
      row.successCount += explicitSuccess ?? 0;
      row.errorCount += explicitErrors ?? 0;
      row.skippedCount = (row.skippedCount ?? 0) + (explicitSkipped ?? 0);
      if (
        typeof trace.resultCount === "number" &&
        Number.isFinite(trace.resultCount)
      ) {
        row.resultCount =
          (row.resultCount ?? 0) + Math.max(0, Math.round(trace.resultCount));
      }
      return;
    }
    const status =
      typeof trace.status === "string" ? trace.status.toLowerCase() : "";
    if (status === "success" || status === "completed" || status === "ok") {
      row.successCount += 1;
    } else if (status === "skipped") {
      row.skippedCount = (row.skippedCount ?? 0) + 1;
    } else if (
      status === "failed" ||
      status === "error" ||
      status === "degraded"
    ) {
      row.errorCount += 1;
    }
    if (
      typeof trace.resultCount === "number" &&
      Number.isFinite(trace.resultCount)
    ) {
      row.resultCount =
        (row.resultCount ?? 0) + Math.max(0, Math.round(trace.resultCount));
    }
  };

  const visit = (node: unknown, depth = 0) => {
    if (depth > 5 || node == null) return;
    if (Array.isArray(node)) {
      for (const item of node) visit(item, depth + 1);
      return;
    }
    if (typeof node !== "object") return;
    const record = node as Record<string, unknown>;
    if (
      typeof record.key === "string" &&
      typeof record.available === "boolean" &&
      typeof record.status === "string"
    ) {
      return;
    } else if (
      (typeof record.layer === "string" ||
        typeof record.sourceType === "string") &&
      (typeof record.status === "string" ||
        typeof record.resultCount === "number" ||
        typeof record.fallbackReason === "string")
    ) {
      addTrace(record);
    }
    for (const key of [
      "searchUsage",
      "searchPipelineUsage",
      "unifiedSearchUsage",
      "searchTrace",
      "searchPipeline",
      "orchestrator",
      "traces",
      "layers",
    ]) {
      if (Object.prototype.hasOwnProperty.call(record, key)) {
        visit(record[key], depth + 1);
      }
    }
  };
  visit(detail);

  for (const item of toolSummary?.byName ?? []) {
    if (item.name === "WebSearch") {
      const row = ensure("web_search_tool");
      if (row) {
        row.attempts += item.count;
        row.errorCount += item.errorCount;
        row.successCount += Math.max(0, item.count - item.errorCount);
      }
    } else if (item.name === "WebFetch") {
      const row = ensure("web_fetch_tool");
      if (row) {
        row.attempts += item.count;
        row.errorCount += item.errorCount;
        row.successCount += Math.max(0, item.count - item.errorCount);
      }
    }
  }

  const ordered = [
    "native_model_search",
    "configured_search_provider",
    "mcp_search",
    "rag_local",
    "web_search_tool",
    "web_fetch_tool",
  ];
  const list = Array.from(rows.values())
    .filter(
      (row) =>
        row.attempts > 0 ||
        row.successCount > 0 ||
        row.errorCount > 0 ||
        (row.skippedCount ?? 0) > 0,
    )
    .sort((a, b) => {
      const ai = ordered.indexOf(a.layer);
      const bi = ordered.indexOf(b.layer);
      if (ai !== -1 || bi !== -1) {
        return (ai === -1 ? 999 : ai) - (bi === -1 ? 999 : bi);
      }
      return a.layer.localeCompare(b.layer);
    });
  return list.length > 0 ? { rows: list } : null;
}

function mergePmSearchUsageSummaries(
  prev: PmSearchUsageSummary | null | undefined,
  incoming: PmSearchUsageSummary | null | undefined,
): PmSearchUsageSummary | null {
  if (!prev && !incoming) return null;
  if (!prev) return incoming ?? null;
  if (!incoming) return prev;
  const map = new Map<string, PmSearchUsageSummary["rows"][number]>();
  for (const row of [...prev.rows, ...incoming.rows]) {
    const current = map.get(row.layer) ?? {
      layer: row.layer,
      label: row.label,
      attempts: 0,
      successCount: 0,
      errorCount: 0,
      skippedCount: 0,
      resultCount: 0,
    };
    current.attempts += row.attempts;
    current.successCount += row.successCount;
    current.errorCount += row.errorCount;
    current.skippedCount =
      (current.skippedCount ?? 0) + (row.skippedCount ?? 0);
    current.resultCount = (current.resultCount ?? 0) + (row.resultCount ?? 0);
    map.set(row.layer, current);
  }
  return { rows: Array.from(map.values()) };
}

const HISTORY_PAGE_LIMIT_TURNS = 8;
const HISTORY_PAGE_MAX_BYTES = 256 * 1024;
const HISTORY_AUTO_LOAD_TOP_THRESHOLD_PX = 72;
const CHAT_INPUT_MIN_HEIGHT_PX = 44;
const CHAT_INPUT_MAX_HEIGHT_PX = 120;
const TYPEWRITER_TICK_MS = 22;
const TYPEWRITER_MIN_CHARS_PER_TICK = 1;
const TYPEWRITER_MAX_CHARS_PER_TICK = 10;
const STREAM_STALL_RECOVERY_IDLE_MS = 15_000;
const STREAM_STALL_RECOVERY_INTERVAL_MS = 5_000;
const PM_PROVIDER_SOURCE_HOSTS = new Set<string>(["api.search.brave.com"]);
function isPmTaskTerminalEvent(event: ApiPmResearchTaskEvent): boolean {
  const stage = (event.stage ?? "").toLowerCase();
  const status = (event.status ?? "").toLowerCase();
  if (stage === "done" || stage === "failed" || stage === "cancelled") {
    return true;
  }
  return status === "completed" && !!event.response;
}

function derivePmBackgroundTaskStatus(event: ApiPmResearchTaskEvent): string {
  if (isPmTaskTerminalEvent(event)) {
    const status = (event.status ?? "").toLowerCase();
    if (status === "failed" || status === "cancelled") {
      return status;
    }
    return "completed";
  }
  const status = (event.status ?? "").toLowerCase();
  if (status === "queued") return "queued";
  if (status === "cancelling") return "cancelling";
  return "running";
}

function isPmTaskTerminalStatus(status: string | null | undefined): boolean {
  const normalized = (status ?? "").toLowerCase();
  return (
    normalized === "completed" ||
    normalized === "clarification_needed" ||
    normalized === "no_data" ||
    normalized === "partial" ||
    normalized === "timed_out" ||
    normalized === "failed" ||
    normalized === "cancelled"
  );
}

function normalizeAttributionTaskStatus(
  status: string | null | undefined,
): string {
  const normalized = (status ?? "").toLowerCase();
  if (
    normalized === "completed" ||
    normalized === "clarification_needed" ||
    normalized === "no_data" ||
    normalized === "partial" ||
    normalized === "timed_out"
  ) {
    return normalized;
  }
  if (normalized === "failed" || normalized === "cancelled") return normalized;
  if (normalized === "queued") return "queued";
  return "running";
}

function normalizeAdversarialRunStatus(
  status: string | null | undefined,
  eventName?: string | null,
): string {
  const normalized = (status ?? "").toLowerCase();
  if (
    normalized === "completed" ||
    normalized === "failed" ||
    normalized === "cancelled"
  ) {
    return normalized;
  }
  if (normalized === "queued" || eventName === "run_queued") return "queued";
  if (normalized === "cancelling" || eventName === "run_cancelled")
    return "cancelling";
  if (eventName === "run_completed" || eventName === "final_completed")
    return "completed";
  if (eventName === "run_failed" || eventName?.endsWith("_failed"))
    return "failed";
  if (eventName === "run_cancelled" || eventName?.endsWith("_cancelled"))
    return "cancelled";
  return "running";
}

function isAdversarialRunTerminalStatus(
  status: string | null | undefined,
): boolean {
  const normalized = (status ?? "").toLowerCase();
  return (
    normalized === "completed" ||
    normalized === "failed" ||
    normalized === "cancelled"
  );
}

function normalizeAdversarialStageStatus(
  status: string | null | undefined,
): PmStageStatus {
  const normalized = (status ?? "").toLowerCase();
  if (normalized === "completed") return "completed";
  if (normalized === "failed" || normalized === "cancelled") return "failed";
  if (normalized === "queued") return "pending";
  return "running";
}

function describeAdversarialEvent(event: ChatAdversarialStreamEvent): string {
  const round = event.round ? `第 ${event.round} 轮` : "";
  if (event.event === "run_queued") return "超级对抗已排队";
  if (event.event === "run_started") return "超级对抗已开始";
  if (event.event === "round_started") return `${round || "新一轮"}开始`;
  if (event.event === "round_completed") return `${round || "本轮"}完成`;
  if (event.event.startsWith("model_")) {
    const state = event.event.endsWith("_completed")
      ? "已完成观点"
      : event.event.endsWith("_failed")
        ? "回复失败"
        : "正在给出观点";
    return `${round ? `${round} · ` : ""}${event.model || "模型"}${state}`;
  }
  if (event.event.startsWith("judge_")) {
    return `${round ? `${round} · ` : ""}裁判模型 ${event.model || "未知"}${
      event.event.endsWith("_completed") ? "已完成裁决" : "正在收敛结论"
    }`;
  }
  if (event.event.startsWith("final_")) {
    return `汇总模型 ${event.model || "未知"}${
      event.event.endsWith("_completed") ? "已完成最终答案" : "正在整理最终答案"
    }`;
  }
  if (event.event === "run_failed") return event.error || "超级对抗执行失败";
  if (event.event === "run_cancelled") return "超级对抗已取消";
  if (event.event === "run_completed") return "超级对抗已完成";
  return event.event;
}

function pickPmDetailString(
  detail: Record<string, unknown> | undefined,
  key: string,
): string {
  const raw = detail?.[key];
  if (typeof raw !== "string") return "";
  return raw.trim();
}

function isPmLightweightChatDetail(
  detail?: Record<string, unknown> | null,
): boolean {
  if (!detail || typeof detail !== "object") return false;
  const route = pickPmDetailString(detail, "route").toLowerCase();
  if (route === "chat_mode") return true;
  if (route === "pm_turn_router_shared_chat_tool_loop") return true;
  const message = pickPmDetailString(detail, "message").toLowerCase();
  if (
    message.includes("共享 chat tool loop") ||
    message.includes("shared chat tool loop")
  ) {
    return true;
  }
  if (detail.sharedChatTurnEngine === true) return true;
  const mode = pickPmDetailString(detail, "mode").toLowerCase();
  if (mode === "chat" && detail.qualityGateSkipped === true) return true;
  return false;
}

function normalizePmStageStatus(input?: string): PmStageStatus {
  if (
    input === "completed" ||
    input === "degraded" ||
    input === "skipped" ||
    input === "failed" ||
    input === "pending"
  ) {
    return input;
  }
  if (input === "cancelled") return "failed";
  return "running";
}

function isPmStageTerminal(status: PmStageStatus | undefined): boolean {
  return (
    status === "completed" ||
    status === "degraded" ||
    status === "skipped" ||
    status === "failed"
  );
}

function normalizePmTaskEventStage(event: ApiPmResearchTaskEvent): string {
  const rawStage = (event.stage ?? "").trim();
  const lowerStage = rawStage.toLowerCase();
  if (
    lowerStage === "done" ||
    lowerStage === "failed" ||
    lowerStage === "cancelled"
  ) {
    return "synthesize";
  }
  return rawStage || "queued";
}

function normalizePmTaskEventStageStatus(
  event: ApiPmResearchTaskEvent,
): PmStageStatus {
  const status = (event.status ?? "").toLowerCase();
  if (status === "completed") return "completed";
  if (status === "degraded") return "degraded";
  if (status === "skipped") return "skipped";
  if (status === "failed" || status === "cancelled") return "failed";
  if (status === "running" || status === "cancelling") return "running";
  return "pending";
}

function normalizePmTaskEventDetail(
  event: ApiPmResearchTaskEvent,
): Record<string, unknown> | undefined {
  const rawDetail =
    event.detail &&
    typeof event.detail === "object" &&
    !Array.isArray(event.detail)
      ? (event.detail as Record<string, unknown>)
      : undefined;
  if (!isPmTaskTerminalEvent(event)) return rawDetail;
  const terminalMessage =
    typeof event.message === "string" && event.message.trim().length > 0
      ? event.message.trim()
      : undefined;
  const terminalError =
    typeof event.error === "string" && event.error.trim().length > 0
      ? event.error.trim()
      : undefined;
  if (!terminalMessage && !terminalError) return rawDetail;
  return {
    ...(rawDetail ?? {}),
    ...(terminalMessage ? { message: terminalMessage } : {}),
    ...(terminalError ? { error: terminalError } : {}),
  };
}

function normalizeAttributionStageStatus(
  event: AttributionTaskEvent,
): PmStageStatus {
  const status = (event.status ?? "").toLowerCase();
  if (
    status === "completed" ||
    status === "clarification_needed" ||
    status === "no_data" ||
    status === "partial"
  ) {
    return "completed";
  }
  if (status === "timed_out" || status === "failed" || status === "cancelled") return "failed";
  if (status === "queued") return "pending";
  return "running";
}

function normalizeAttributionStage(event: AttributionTaskEvent): string {
  const stage = (event.stage ?? "").trim();
  if (!stage && event.status === "completed") return "synthesize";
  if (!stage && event.status === "failed") return "failed";
  return stage || "queued";
}

function normalizeAttributionDetail(
  event: AttributionTaskEvent,
): Record<string, unknown> {
  const observation = event.observation;
  return {
    ...(event.detail ?? {}),
    source: "data_attribution",
    message: event.message ?? undefined,
    error: event.error ?? undefined,
    progressPercent: event.progress_percent ?? undefined,
    stepIndex: event.step_index ?? undefined,
    stepTotal: event.step_total ?? undefined,
    observationTitle: observation?.title,
    observationPurpose: observation?.purpose,
    observationQuestion: observation?.question,
    rowCount: observation?.rowCount,
    sqlCount: observation?.sqls?.length,
    sqls: observation?.sqls,
    columns: observation?.columns,
    rowsPreview: observation?.rows?.slice(0, 12),
    usedReferences: observation?.usedReferences,
    sampled: observation?.sampled,
  };
}

function formatAttributionTerminalMessage(event: AttributionTaskEvent): string {
  const response = event.response;
  if (event.status === "failed" || event.status === "timed_out") {
    return event.error || event.message || "数据归因执行失败。";
  }
  if (!response) {
    return event.message || "数据归因任务已结束。";
  }
  if (response.clarificationQuestion) {
    return `需要补充信息：${response.clarificationQuestion}`;
  }
  const report = response.report;
  const lines: string[] = [];
  if (report?.title) lines.push(`## ${report.title}`);
  if (report?.executiveSummary) lines.push(report.executiveSummary);
  if (report?.metricAnswer)
    lines.push(`\n**核心结论**\n${report.metricAnswer}`);
  if (report?.mainCauses?.length) {
    lines.push("\n**主要原因**");
    for (const cause of report.mainCauses.slice(0, 5)) {
      lines.push(`- ${cause.title}: ${cause.explanation}`);
    }
  }
  if (report?.recommendations?.length) {
    lines.push("\n**建议动作**");
    for (const item of report.recommendations.slice(0, 5)) {
      lines.push(`- ${item}`);
    }
  }
  if (report?.caveats?.length) {
    lines.push("\n**注意事项**");
    for (const item of report.caveats.slice(0, 3)) {
      lines.push(`- ${item}`);
    }
  }
  const health = response.evidenceHealth;
  if (health) {
    lines.push(
      `\n证据覆盖：成功 ${health.successfulSteps}/${health.totalSteps} 步，返回 ${health.totalRows} 行。`,
    );
  }
  const sqls = Array.from(
    new Set(
      response.observations
        .flatMap((observation) => observation.sqls ?? [])
        .map((sql) => sql.trim())
        .filter(Boolean),
    ),
  ).slice(0, 8);
  if (sqls.length > 0) {
    lines.push("\n## 已执行 SQL");
    sqls.forEach((sql, index) => {
      if (sqls.length > 1) lines.push(`\n**SQL ${index + 1}**`);
      lines.push(`\n\`\`\`sql\n${sql.slice(0, 12_000)}\n\`\`\``);
    });
  }
  return lines.join("\n").trim() || response.error || "数据归因任务已完成。";
}

function tryParseLooseJson(raw: string): unknown | null {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    // continue
  }
  const objectMatches = raw.match(/\{[\s\S]*\}/g);
  if (objectMatches) {
    for (let i = objectMatches.length - 1; i >= 0; i -= 1) {
      try {
        return JSON.parse(objectMatches[i]);
      } catch {
        // continue
      }
    }
  }
  const arrayMatches = raw.match(/\[[\s\S]*\]/g);
  if (arrayMatches) {
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

function shortHumanText(input: string, max = 88): string {
  const compact = input.trim().replace(/\s+/g, " ");
  if (compact.length <= max) return compact;
  return `${compact.slice(0, max - 1)}...`;
}

function isPmInternalContractLine(line: string): boolean {
  const trimmed = line.trim();
  if (!trimmed) return false;
  return (
    /^(TURN_ROUTE|TASK_GRAPH_V2|TASK_GRAPH|TASK_DECOMPOSITION|EXEC_CONSTRAINTS)\b/i.test(
      trimmed,
    ) ||
    /^<\/?(EXEC_CONSTRAINTS|RETRIEVE_CONSTRAINTS)>/i.test(trimmed) ||
    (/^\{/.test(trimmed) &&
      (/"turnClass"\s*:/.test(trimmed) ||
        /"domainScope"\s*:/.test(trimmed) ||
        /"decompositionMode"\s*:/.test(trimmed) ||
        /"routeAllowlist"\s*:/.test(trimmed)))
  );
}

function sanitizePmUserFacingStageText(input: string): string {
  const lines = input.split(/\r?\n/);
  const kept: string[] = [];
  for (const line of lines) {
    if (isPmInternalContractLine(line)) {
      break;
    }
    kept.push(line);
  }
  return kept.join("\n").trim();
}

function normalizePmNarrativeText(input: string, max = 560): string {
  const cleaned = input
    .split(/\n{2,}/)
    .map(sanitizePmUserFacingStageText)
    .filter(Boolean)
    .join("\n\n")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(
      (line) =>
        line.length > 0 &&
        !/^thinking[:：]/i.test(line) &&
        !/^思考中[:：]?/i.test(line),
    )
    .join("\n");
  if (cleaned.length <= max) return cleaned;
  return `${cleaned.slice(0, max - 1)}...`;
}

function normalizePmNarrativeDedupKey(input: string): string {
  return input
    .normalize("NFKC")
    .toLowerCase()
    .replace(/\s+/g, "")
    .replace(/[，。、“”‘’：:；;,.!?！？（）()【】\x5B\x5D{}<>《》\-_/\\|`~…]/g, "");
}

function mergePmNarrativeBlocks(blocks: string[]): string {
  const merged: string[] = [];
  for (const raw of blocks) {
    const candidate = raw.trim();
    if (!candidate) continue;
    const candidateKey = normalizePmNarrativeDedupKey(candidate);
    if (!candidateKey) continue;
    let handled = false;
    for (let i = 0; i < merged.length; i += 1) {
      const existing = merged[i];
      const existingKey = normalizePmNarrativeDedupKey(existing);
      if (!existingKey) continue;
      if (existingKey.includes(candidateKey)) {
        handled = true;
        break;
      }
      if (candidateKey.includes(existingKey)) {
        merged[i] = candidate;
        handled = true;
        break;
      }
      const minLen = Math.min(existingKey.length, candidateKey.length);
      if (minLen >= 120) {
        const overlapLen = Math.floor(minLen * 0.72);
        if (
          existingKey.slice(0, overlapLen) === candidateKey.slice(0, overlapLen)
        ) {
          if (candidateKey.length > existingKey.length) {
            merged[i] = candidate;
          }
          handled = true;
          break;
        }
      }
    }
    if (!handled) {
      merged.push(candidate);
    }
  }
  return merged.join("\n\n");
}

function scalarToText(value: unknown): string | null {
  if (typeof value === "string") return shortHumanText(value, 140);
  if (typeof value === "number" || typeof value === "boolean")
    return String(value);
  if (Array.isArray(value)) {
    const head = value
      .slice(0, 3)
      .map((item) => (typeof item === "string" ? item : ""))
      .filter(Boolean)
      .join(" / ");
    return head ? shortHumanText(head, 140) : null;
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
    if (normalized.has(key.toLowerCase())) {
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

function uniqueNonEmptyStrings(values: unknown[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const value of values) {
    if (typeof value !== "string") continue;
    const trimmed = value.trim();
    if (!trimmed) continue;
    const key = trimmed.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(trimmed);
  }
  return out;
}

function sessionMetadataNames(raw: unknown): string[] {
  return Array.isArray(raw) ? uniqueNonEmptyStrings(raw) : [];
}

function enabledMcpServerNames(data: unknown): string[] {
  const servers = (
    data as
      { servers?: Array<{ name?: unknown; enabled?: unknown }> } | undefined
  )?.servers;
  if (!Array.isArray(servers)) return [];
  return uniqueNonEmptyStrings(
    servers
      .filter((server) => server?.enabled === true)
      .map((server) => server.name),
  );
}

function enabledSkillNamesFromList(data: unknown): string[] {
  const skills = (
    data as
      { skills?: Array<{ name?: unknown; enabled?: unknown }> } | undefined
  )?.skills;
  if (!Array.isArray(skills)) return [];
  return uniqueNonEmptyStrings(
    skills
      .filter((skill) => skill?.enabled === true)
      .map((skill) => skill.name),
  );
}

function normalizePmTargetCandidate(input: string, max = 96): string | null {
  const compact = input.trim().replace(/\s+/g, " ");
  if (!compact) return null;
  // Filter out index-like placeholders such as "0", "'0'", "['0']", "[0]".
  if (
    /^['"]?\d+['"]?$/.test(compact) ||
    /^\[\s*['"]?\d+['"]?\s*\]$/.test(compact)
  ) {
    return null;
  }
  if (/^[\x5B\x5D{}(),.:;"'`]+$/.test(compact)) return null;
  if (/^(null|undefined)$/i.test(compact)) return null;
  if (
    /^[A-Za-z_][\w-]{1,64}$/.test(compact) &&
    /(search|fetch|browser|web|tool|mcp|skill)/i.test(compact)
  ) {
    return null;
  }
  const commandLike =
    compact.length > 180 ||
    /(python3?\s+-|<<'PY|import\s+[a-zA-Z_][\w.]*|def\s+[a-zA-Z_]\w*\(|curl\s+https?:\/\/|bash\s+-c|node\s+-e|SELECT\s+.+\s+FROM\s+)/i.test(
      compact,
    );
  if (commandLike) return null;
  return shortHumanText(compact, max);
}

function extractPmTargetFromArgs(rawArgs: string): string | null {
  const parsed = tryParseLooseJson(rawArgs);
  if (Array.isArray(parsed)) {
    for (const item of parsed) {
      if (!item || typeof item !== "object" || Array.isArray(item)) continue;
      const nested = findByKeys(item, [
        "query",
        "q",
        "keyword",
        "keywords",
        "question",
        "prompt",
        "topic",
        "term",
        "title",
        "url",
        "urls",
        "uri",
        "site",
        "domain",
        "target",
        "country",
        "market",
      ]);
      if (!nested) continue;
      const normalized = normalizePmTargetCandidate(nested, 96);
      if (normalized) return normalized;
    }
  }
  const fromJsonRaw = Array.isArray(parsed)
    ? null
    : findByKeys(parsed, [
        "query",
        "q",
        "keyword",
        "keywords",
        "question",
        "prompt",
        "topic",
        "term",
        "title",
        "url",
        "urls",
        "uri",
        "site",
        "domain",
        "target",
        "country",
        "market",
      ]);
  const fromJson = fromJsonRaw
    ? normalizePmTargetCandidate(fromJsonRaw, 96)
    : null;
  if (fromJson) return fromJson;
  const queryMatch =
    rawArgs.match(/"query"\s*:\s*"([^"]+)"/i) ??
    rawArgs.match(/"q"\s*:\s*"([^"]+)"/i);
  if (queryMatch?.[1]) {
    const normalized = normalizePmTargetCandidate(queryMatch[1], 96);
    if (normalized) return normalized;
  }
  const urlMatch = rawArgs.match(/https?:\/\/[^\s"']+/i);
  if (urlMatch?.[0]) {
    const normalized = normalizePmTargetCandidate(urlMatch[0], 96);
    if (normalized) return normalized;
  }
  const looseMatch = rawArgs.match(
    /(?:query|q|keyword|keywords|prompt|url|site|domain|target)\s*[:=]\s*["']?([^"',}\n]+)/i,
  );
  if (looseMatch?.[1]) {
    const normalized = normalizePmTargetCandidate(looseMatch[1].trim(), 96);
    if (normalized) return normalized;
  }
  const normalizedRaw = normalizePmTargetCandidate(rawArgs, 96);
  if (normalizedRaw) return normalizedRaw;
  return null;
}

function extractPmTargetFromDetail(
  detail?: Record<string, unknown>,
): string | null {
  if (!detail) return null;
  const messageText =
    typeof detail.message === "string" ? detail.message.trim() : "";
  if (messageText) {
    const quotedMatch = messageText.match(/[「“"]([^」”"]+)[」”"]/);
    if (quotedMatch?.[1]) {
      const normalized = normalizePmTargetCandidate(quotedMatch[1], 96);
      if (normalized) return normalized;
    }
  }
  const target = findByKeys(detail, [
    "target",
    "query",
    "q",
    "keyword",
    "keywords",
    "topic",
    "prompt",
    "url",
    "uri",
    "site",
    "domain",
    "selectedVariant",
    "selectedQuery",
    "inputPreview",
  ]);
  if (target) {
    const normalized = normalizePmTargetCandidate(target, 96);
    if (normalized) return normalized;
  }
  const rawInputCandidates: unknown[] = [
    detail.input,
    detail.args,
    detail.inputPreview,
    detail.payload,
  ];
  for (const candidate of rawInputCandidates) {
    if (typeof candidate !== "string" || candidate.trim().length === 0) {
      continue;
    }
    const parsed = extractPmTargetFromArgs(candidate);
    if (parsed) return parsed;
    const normalized = normalizePmTargetCandidate(candidate, 96);
    if (normalized) return normalized;
  }
  return null;
}

function summarizePmToolResult(rawResult: string): string | null {
  const visibleResult = rawResult
    .replace(/\s*Hook feedback(?: \(error\))?:[\s\S]*$/i, "")
    .trim();
  if (/unsupported tool:\s*(?:web_search|web-search)\b/i.test(visibleResult)) {
    return null;
  }
  const parsed = tryParseLooseJson(visibleResult);
  const count = countFromKeys(parsed, [
    "results",
    "items",
    "sources",
    "documents",
    "hits",
    "rows",
    "posts",
    "comments",
    "reviews",
  ]);
  if (count != null) return `#${count}`;
  const preview = findByKeys(parsed, [
    "summary",
    "snippet",
    "content",
    "text",
    "message",
    "reason",
    "error",
    "title",
  ]);
  if (preview) {
    const normalized = normalizePmTargetCandidate(preview, 110);
    if (normalized) return normalized;
  }
  if (visibleResult) {
    const normalized = normalizePmTargetCandidate(visibleResult, 110);
    if (normalized) return normalized;
  }
  return null;
}

function inferToolStageForPm(
  rawName: string,
): "search" | "extract" | "verify" | "analyze" | "synthesize" | "execute" {
  const n = rawName.toLowerCase();
  if (/(web|search|query|lookup|find)/.test(n)) return "search";
  if (/(extract|scrape|crawl|fetch|visit|open|read|download|browser)/.test(n))
    return "extract";
  if (/(verify|validate|fact|check|ground|evidence)/.test(n)) return "verify";
  if (/(cluster|rank|score|classify|analy|analysis)/.test(n)) return "analyze";
  if (/(summar|synth|compose|write|draft|report|answer)/.test(n))
    return "synthesize";
  return "execute";
}

interface PmStageTimingHint {
  nowMs?: number;
  runningSinceMs?: number;
  pipelineStartedAtMs?: number;
}

function asFiniteNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  return null;
}

function formatBudgetMs(ms: number): string {
  const clamped = Math.max(0, ms);
  if (clamped < 1000) {
    return `${Math.round(clamped)}ms`;
  }
  const secs = clamped / 1000;
  return secs >= 10 ? `${secs.toFixed(0)}s` : `${secs.toFixed(1)}s`;
}

function appendBudgetDetail(
  stage: string,
  stageStatus: PmStageStatus | undefined,
  baseText: string,
  detail?: Record<string, unknown>,
  timingHint?: PmStageTimingHint,
): string {
  const nowMs = timingHint?.nowMs ?? Date.now();
  const openEndedTiming =
    detail?.timingPolicy === "open_ended" || stage === "report_extract";
  const stageBudgetMs = openEndedTiming
    ? null
    : (asFiniteNumber(detail?.stageBudgetMs) ??
      (Object.prototype.hasOwnProperty.call(PM_STAGE_BUDGET_MS, stage)
        ? PM_STAGE_BUDGET_MS[stage as PmStageId]
        : null));
  const pipelineBudgetMs = openEndedTiming
    ? null
    : (asFiniteNumber(detail?.pipelineBudgetMs) ?? PM_PIPELINE_BUDGET_MS);
  const stageElapsedMs =
    asFiniteNumber(detail?.stageElapsedMs) ??
    (stageStatus === "running" && timingHint?.runningSinceMs != null
      ? Math.max(0, nowMs - timingHint.runningSinceMs)
      : null);
  const stageRemainingMs =
    asFiniteNumber(detail?.stageRemainingMs) ??
    (stageBudgetMs != null && stageElapsedMs != null
      ? Math.max(0, stageBudgetMs - stageElapsedMs)
      : null);
  const pipelineElapsedMs =
    asFiniteNumber(detail?.pipelineElapsedMs) ??
    (stageStatus === "running" && timingHint?.pipelineStartedAtMs != null
      ? Math.max(0, nowMs - timingHint.pipelineStartedAtMs)
      : null);
  const pipelineRemainingMs =
    asFiniteNumber(detail?.pipelineRemainingMs) ??
    (pipelineBudgetMs != null && pipelineElapsedMs != null
      ? Math.max(0, pipelineBudgetMs - pipelineElapsedMs)
      : null);

  const elapsedText =
    stageElapsedMs != null ? `已耗时 ${formatBudgetMs(stageElapsedMs)}` : "";
  const overBudget =
    stageBudgetMs != null &&
    stageElapsedMs != null &&
    stageElapsedMs > stageBudgetMs;
  const remainingText =
    stageRemainingMs != null
      ? `剩余预算 ${formatBudgetMs(stageRemainingMs)}`
      : pipelineRemainingMs != null
        ? `剩余预算 ${formatBudgetMs(pipelineRemainingMs)}`
        : "";
  const timingText = [
    elapsedText,
    remainingText,
    overBudget ? "已超预算，等待收敛" : "",
  ]
    .filter(Boolean)
    .join(" · ");
  if (!timingText) return baseText;
  if (!baseText || !baseText.trim()) return timingText;
  return `${baseText} · ${timingText}`;
}

function toReadableStageDetail(
  stage: string,
  detail?: Record<string, unknown>,
  timingHint?: PmStageTimingHint,
  stageStatus?: PmStageStatus,
): string {
  if (!detail) return "";
  if (
    typeof detail.result === "string" &&
    detail.result.includes("degraded_answer_delivered")
  ) {
    // User-facing stage details should focus on progress/timing, not "degraded" internals.
    const { result: _drop, ...rest } = detail;
    detail = rest;
  }
  const withBudget = (text: string): string =>
    appendBudgetDetail(stage, stageStatus, text, detail, timingHint);

  if (detail.mode === "chat") {
    const query =
      typeof detail.query === "string" ? shortHumanText(detail.query, 68) : "";
    return withBudget(query ? `轻量对话模式 · ${query}` : "轻量对话模式");
  }

  const selectedVariant =
    typeof detail.selectedVariant === "string" &&
    detail.selectedVariant.trim().length > 0
      ? shortHumanText(detail.selectedVariant, 72)
      : null;
  const sourceRouteCount =
    typeof detail.sourceRouteCount === "number"
      ? detail.sourceRouteCount
      : null;
  const queryVariantCount =
    typeof detail.queryVariantCount === "number"
      ? detail.queryVariantCount
      : null;
  const probeCount =
    typeof detail.probeCount === "number" ? detail.probeCount : null;
  const toolCallCount =
    typeof detail.toolCallCount === "number" ? detail.toolCallCount : null;
  const domainCount =
    typeof detail.domainCount === "number" ? detail.domainCount : null;
  const citationCount =
    typeof detail.citationCount === "number" ? detail.citationCount : null;
  const passed = detail.passed === true;
  const qualityGatePassed = detail.qualityGatePassed === true;
  const deliverable = detail.deliverable === true;
  const qualityLevel =
    typeof detail.qualityLevel === "string" ? detail.qualityLevel : null;
  const humanSummary =
    typeof detail.humanSummary === "string" &&
    detail.humanSummary.trim().length > 0
      ? shortHumanText(
          sanitizePmUserFacingStageText(detail.humanSummary.trim()),
          120,
        )
      : null;
  const preview =
    typeof detail.preview === "string" && detail.preview.trim().length > 0
      ? shortHumanText(
          sanitizePmUserFacingStageText(detail.preview.trim()),
          120,
        )
      : null;
  const errorCode =
    typeof detail.error === "string" && detail.error.trim().length > 0
      ? detail.error.trim()
      : null;

  const progressNarrative =
    typeof detail.progressNarrative === "string" &&
    detail.progressNarrative.trim().length > 0
      ? shortHumanText(detail.progressNarrative.trim(), 220)
      : null;
  if (progressNarrative) return withBudget(progressNarrative);

  const executionDetail =
    detail.executionDetail &&
    typeof detail.executionDetail === "object" &&
    !Array.isArray(detail.executionDetail)
      ? (detail.executionDetail as Record<string, unknown>)
      : null;
  if (stage.startsWith("nl2sql_") && executionDetail) {
    const queryId = typeof executionDetail.queryId === "string"
      ? executionDetail.queryId
      : null;
    const queryStatus = typeof executionDetail.status === "string"
      ? executionDetail.status
      : null;
    const processedRows = typeof executionDetail.processedRows === "number"
      ? executionDetail.processedRows
      : null;
    const rowCount = typeof executionDetail.rowCount === "number"
      ? executionDetail.rowCount
      : null;
    return withBudget([
      queryStatus ? `状态 ${queryStatus}` : "",
      queryId ? `Query ${queryId}` : "",
      processedRows != null ? `已处理 ${processedRows} 行` : "",
      rowCount != null ? `返回 ${rowCount} 行` : "",
    ].filter(Boolean).join(" · "));
  }

  if ((stage === "understand" || stage === "task_plan") && humanSummary) {
    return withBudget(humanSummary);
  }

  if ((stage === "understand" || stage === "task_plan") && preview) {
    return withBudget(preview);
  }

  if (stage === "report_extract") {
    const message =
      typeof detail.message === "string" && detail.message.trim().length > 0
        ? shortHumanText(
            sanitizePmUserFacingStageText(detail.message.trim()),
            120,
          )
        : null;
    const applied = detail.applied === true;
    const keySentenceCount =
      typeof detail.keySentenceCount === "number"
        ? detail.keySentenceCount
        : null;
    const searchQueryCount =
      typeof detail.searchQueryCount === "number"
        ? detail.searchQueryCount
        : null;
    const parts = [
      message,
      keySentenceCount != null ? `关键句 ${keySentenceCount} 条` : "",
      searchQueryCount != null ? `检索主题 ${searchQueryCount} 个` : "",
      detail.toolPolicy === "disabled" ? "不调用工具" : "",
      detail.degraded === true
        ? "模型提取未命中，继续使用本地解析"
        : applied
          ? "已更新研究线索"
          : "",
    ].filter(Boolean);
    if (parts.length > 0) return withBudget(parts.join(" · "));
  }

  if (stage === "preflight") {
    const modelOk = detail.modelStreamOk === true;
    const modelSoftTimeoutAllowed = detail.modelSoftTimeoutAllowed === true;
    const retrievalSoftFailureAllowed =
      detail.retrievalSoftFailureAllowed === true;
    const retrievalOk =
      detail.retrievalEgressOk === true || detail.requireRetrieval === false;
    const modelLatency =
      typeof detail.modelLatencyMs === "number" ? detail.modelLatencyMs : null;
    const retrievalLatency =
      typeof detail.retrievalLatencyMs === "number"
        ? detail.retrievalLatencyMs
        : null;
    const cached = detail.cached === true;
    const modelError =
      typeof detail.modelError === "string" ? detail.modelError : null;
    const retrievalError =
      typeof detail.retrievalError === "string" ? detail.retrievalError : null;

    if (!modelOk && modelSoftTimeoutAllowed) {
      const parts = [
        "模型预检超时（已容错）",
        retrievalOk ? "检索链路可用" : "检索链路异常",
        retrievalLatency != null ? `检索延迟 ${retrievalLatency}ms` : "",
        cached ? "使用缓存结果" : "",
      ].filter(Boolean);
      return withBudget(parts.join(" · "));
    }

    if (!retrievalOk && retrievalSoftFailureAllowed) {
      const parts = [
        "检索预检失败（已容错继续）",
        retrievalError ? shortHumanText(retrievalError, 96) : "",
        cached ? "使用缓存结果" : "",
      ].filter(Boolean);
      return withBudget(parts.join(" · "));
    }

    if (!modelOk && modelError) {
      return withBudget("模型流式通道暂不可用，正在自动恢复");
    }
    if (!retrievalOk && retrievalError) {
      return withBudget("检索网络暂不可用，正在切换可用来源");
    }
    const parts = [
      modelOk ? "模型可用" : "模型不可用",
      detail.requireRetrieval === false
        ? "跳过检索"
        : retrievalOk
          ? "检索链路可用"
          : "检索链路异常",
      modelLatency != null ? `模型延迟 ${modelLatency}ms` : "",
      retrievalLatency != null ? `检索延迟 ${retrievalLatency}ms` : "",
      cached ? "使用缓存结果" : "",
    ].filter(Boolean);
    if (parts.length > 0) return withBudget(parts.join(" · "));
  }

  if (stage === "retrieve") {
    const message =
      typeof detail.message === "string" && detail.message.trim().length > 0
        ? shortHumanText(detail.message.trim(), 120)
        : null;
    const nativeWebSearch =
      detail.nativeWebSearch &&
      typeof detail.nativeWebSearch === "object" &&
      !Array.isArray(detail.nativeWebSearch)
        ? (detail.nativeWebSearch as Record<string, unknown>)
        : null;
    const nativeWebSearchPreferred = nativeWebSearch?.preferred === true;
    const nativeWebSearchStatus =
      typeof nativeWebSearch?.status === "string"
        ? nativeWebSearch.status
        : null;
    const nativeWebSearchText = nativeWebSearchPreferred
      ? nativeWebSearchStatus === "rejected"
        ? "模型原生联网被拒绝，已降级"
        : nativeWebSearchStatus === "accepted"
          ? "模型原生联网已接受"
          : "模型原生联网优先"
      : nativeWebSearchStatus
        ? "统一联网链路已启动"
        : "";
    const usedLayer =
      typeof detail.usedLayer === "string" && detail.usedLayer.trim().length > 0
        ? (PM_SEARCH_LAYER_LABELS[normalizeSearchLayerName(detail.usedLayer)] ??
          detail.usedLayer.trim())
        : null;
    const nativeAttempts =
      typeof detail.nativeAttempts === "number" ? detail.nativeAttempts : null;
    const configuredProviderAttempts =
      typeof detail.configuredProviderAttempts === "number"
        ? detail.configuredProviderAttempts
        : null;
    const mcpAttempts =
      typeof detail.mcpAttempts === "number" ? detail.mcpAttempts : null;
    const ragLocalAttempts =
      typeof detail.ragLocalAttempts === "number"
        ? detail.ragLocalAttempts
        : null;
    const searchAttemptParts = [
      nativeAttempts != null ? `模型原生 ${nativeAttempts} 次` : "",
      configuredProviderAttempts != null
        ? `Search 扩展 ${configuredProviderAttempts} 次`
        : "",
      mcpAttempts != null ? `MCP ${mcpAttempts} 次` : "",
      ragLocalAttempts != null ? `RAG/local ${ragLocalAttempts} 次` : "",
    ].filter(Boolean);
    if (errorCode) {
      if (errorCode === "network_request_failed_fast_exit") {
        return withBudget("当前来源网络受阻，已快速失败并切换来源");
      }
      if (errorCode === "tool_only_no_text") {
        return withBudget("已完成工具调用，但模型未产出结论文本，正在自动修复");
      }
      if (
        errorCode.includes("all API keys failed") ||
        errorCode.includes("empty response from model")
      ) {
        return withBudget("模型上游返回空响应，正在自动切换与重试");
      }
      if (errorCode.includes("pipeline_timeout_after_")) {
        return withBudget("研究总预算已到，正在快速收敛输出可用结论");
      }
      if (errorCode.includes("retrieve turn timed out")) {
        return withBudget("当前来源超时，正在切换修复策略");
      }
    }
    const parts = [
      nativeWebSearchText,
      selectedVariant ? `聚焦问题「${selectedVariant}」` : "",
      usedLayer ? `采用 ${usedLayer}` : "",
      searchAttemptParts.length > 0
        ? `调用 ${searchAttemptParts.join(" / ")}`
        : "",
      probeCount != null ? `并行探测 ${probeCount} 次` : "",
      toolCallCount != null ? `工具调用 ${toolCallCount} 次` : "",
    ].filter(Boolean);
    if (message && parts.length > 0)
      return withBudget(`${message} · ${parts.join(" · ")}`);
    if (message) return withBudget(message);
    if (parts.length > 0) return withBudget(parts.join(" · "));
  }

  if (stage === "planner") {
    const parts = [
      queryVariantCount != null ? `生成查询变体 ${queryVariantCount} 个` : "",
      sourceRouteCount != null ? `候选来源 ${sourceRouteCount} 条` : "",
    ].filter(Boolean);
    if (parts.length > 0) return withBudget(parts.join(" · "));
  }

  if (stage === "deep_loop") {
    const loopState =
      typeof detail.loopState === "string" ? detail.loopState : null;
    const decision =
      detail.decision &&
      typeof detail.decision === "object" &&
      !Array.isArray(detail.decision)
        ? (detail.decision as Record<string, unknown>)
        : null;
    const action =
      typeof decision?.action === "string" ? decision.action : null;
    const reason =
      typeof decision?.reason === "string" ? decision.reason : null;
    const scores =
      detail.scores &&
      typeof detail.scores === "object" &&
      !Array.isArray(detail.scores)
        ? (detail.scores as Record<string, unknown>)
        : null;
    const readiness =
      typeof scores?.decisionReadinessScore === "number"
        ? Math.round(scores.decisionReadinessScore * 100)
        : null;
    const parts = [
      loopState ? `Loop ${loopState}` : "",
      action ? `决策 ${action}` : "",
      readiness != null ? `就绪度 ${readiness}%` : "",
      reason ? shortHumanText(reason, 72) : "",
    ].filter(Boolean);
    if (parts.length > 0) return withBudget(parts.join(" · "));
  }

  if (stage === "verify") {
    const parts = [
      toolCallCount != null ? `工具调用 ${toolCallCount} 次` : "",
      domainCount != null ? `覆盖站点 ${domainCount} 个` : "",
      citationCount != null ? `引用链接 ${citationCount} 条` : "",
      qualityLevel === "high"
        ? "高置信"
        : qualityLevel === "partial"
          ? "部分可交付"
          : "",
      detail.passed === true || detail.passed === false
        ? passed
          ? "质量通过"
          : "需要修复"
        : "",
    ].filter(Boolean);
    if (parts.length > 0) return withBudget(parts.join(" · "));
  }

  if (stage === "retry_repair") {
    const message =
      typeof detail.message === "string" && detail.message.trim().length > 0
        ? shortHumanText(detail.message.trim(), 120)
        : null;
    const nextRoute =
      typeof detail.nextRoute === "string" && detail.nextRoute.trim().length > 0
        ? detail.nextRoute.trim()
        : null;
    const nextVariant =
      typeof detail.nextVariant === "string" &&
      detail.nextVariant.trim().length > 0
        ? shortHumanText(detail.nextVariant.trim(), 72)
        : null;
    const reasonList = Array.isArray(detail.reason)
      ? (detail.reason as unknown[])
          .filter((item) => typeof item === "string")
          .map((item) => String(item))
      : [];
    if (message || reasonList.length > 0 || nextRoute || nextVariant) {
      return withBudget(
        [
          message ?? "",
          nextRoute ? `切换来源 ${nextRoute}` : "",
          nextVariant ? `补检问题「${nextVariant}」` : "",
          reasonList.length > 0 ? "正在补齐关键证据" : "",
        ]
          .filter(Boolean)
          .join(" · "),
      );
    }
  }

  if (stage === "synthesize") {
    const answerLength =
      typeof detail.answerLength === "number" &&
      Number.isFinite(detail.answerLength)
        ? Math.max(0, Math.round(detail.answerLength))
        : null;
    const parts = [
      answerLength != null ? `结论长度 ${answerLength} 字符` : "",
      qualityLevel === "high"
        ? "高置信结论"
        : qualityLevel === "partial"
          ? "部分证据结论"
          : "",
      detail.deliverable === true ? "已产出可交付结论" : "",
      detail.qualityGatePassed === true || detail.qualityGatePassed === false
        ? qualityGatePassed
          ? "质量通过"
          : deliverable || qualityLevel === "partial" || (answerLength ?? 0) > 0
            ? "证据覆盖部分达标（结论已降级交付）"
            : stageStatus === "running"
              ? "正在补齐证据并校验结论"
              : "质量校验未达到交付标准"
        : "",
    ].filter(Boolean);
    if (parts.length > 0) return withBudget(parts.join(" · "));
  }

  const plan = detail.plan;
  if (plan && typeof plan === "object" && !Array.isArray(plan)) {
    const planRecord = plan as Record<string, unknown>;
    const variants = Array.isArray(planRecord.queryVariants)
      ? planRecord.queryVariants
      : [];
    const enabledRoutes = Array.isArray(planRecord.sourceRoutes)
      ? (planRecord.sourceRoutes as Record<string, unknown>[]).filter(
          (item) => item?.enabled !== false,
        )
      : [];
    const variantsBrief = variants
      .slice(0, 2)
      .map((item) => String(item))
      .map((item) => shortHumanText(item, 34))
      .join(" / ");
    const routesBrief = enabledRoutes
      .slice(0, 2)
      .map((item) => String(item.routeId ?? item.channel ?? "route"))
      .join(" / ");
    const summaryParts = [
      variants.length > 0 ? `查询变体 ${variants.length} 个` : "",
      enabledRoutes.length > 0 ? `来源路由 ${enabledRoutes.length} 条` : "",
    ]
      .filter(Boolean)
      .join(" · ");
    if (summaryParts) {
      const suffix = [variantsBrief, routesBrief].filter(Boolean).join(" · ");
      return withBudget(suffix ? `${summaryParts} · ${suffix}` : summaryParts);
    }
  }

  if (
    probeCount != null ||
    toolCallCount != null ||
    domainCount != null ||
    citationCount != null
  ) {
    const bits = [
      probeCount != null ? `并行探测 ${probeCount} 次` : "",
      toolCallCount != null ? `工具调用 ${toolCallCount} 次` : "",
      domainCount != null ? `站点覆盖 ${domainCount}` : "",
      citationCount != null ? `引用链接 ${citationCount}` : "",
      detail.passed === true || detail.passed === false
        ? passed
          ? "质量通过"
          : deliverable
            ? "可交付（部分证据）"
            : "质量修复中"
        : "",
    ]
      .filter(Boolean)
      .join(" · ");
    if (bits) return withBudget(bits);
  }

  const keys = [
    "reason",
    "strategy",
    "message",
    "error",
    "query",
    "source",
    "route",
    "preview",
  ];
  for (const key of keys) {
    const value = detail[key];
    if (typeof value === "string" && value.trim().length > 0) {
      return withBudget(shortHumanText(value.trim(), 96));
    }
  }

  const missing = detail.missing;
  if (Array.isArray(missing) && missing.length > 0) {
    return withBudget(`待补齐项: ${missing.slice(0, 3).join(" / ")}`);
  }

  return "";
}

function parsePmToolSummary(
  detail?: Record<string, unknown>,
): PmToolSummary | null {
  if (!detail || typeof detail !== "object") return null;
  const raw = (detail as Record<string, unknown>).toolSummary;
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const record = raw as Record<string, unknown>;

  const count =
    typeof record.count === "number" && Number.isFinite(record.count)
      ? Math.max(0, Math.round(record.count))
      : 0;

  const byNameMapRaw =
    record.byName &&
    typeof record.byName === "object" &&
    !Array.isArray(record.byName)
      ? (record.byName as Record<string, unknown>)
      : {};
  const byNameErrorMapRaw =
    record.byNameError &&
    typeof record.byNameError === "object" &&
    !Array.isArray(record.byNameError)
      ? (record.byNameError as Record<string, unknown>)
      : {};
  const errorCount =
    typeof record.errorCount === "number" && Number.isFinite(record.errorCount)
      ? Math.max(0, Math.round(record.errorCount))
      : 0;
  const byName = Object.entries(byNameMapRaw)
    .map(([name, value]) => ({
      name,
      count:
        typeof value === "number" && Number.isFinite(value)
          ? Math.max(0, Math.round(value))
          : 0,
      errorCount:
        typeof byNameErrorMapRaw[name] === "number" &&
        Number.isFinite(byNameErrorMapRaw[name])
          ? Math.max(0, Math.round(byNameErrorMapRaw[name] as number))
          : 0,
    }))
    .filter((item) => item.count > 0)
    .sort((a, b) => b.count - a.count)
    .slice(0, 8);

  const samplesRaw = Array.isArray(record.samples) ? record.samples : [];
  const samples: PmToolSummarySample[] = samplesRaw
    .filter((item) => item && typeof item === "object")
    .map((item) => {
      const row = item as Record<string, unknown>;
      return {
        idx:
          typeof row.idx === "number" && Number.isFinite(row.idx)
            ? Math.max(0, Math.round(row.idx))
            : 0,
        tool: typeof row.tool === "string" ? row.tool : "unknown",
        source: typeof row.source === "string" ? row.source : undefined,
        isError: row.isError === true,
        durationMs:
          typeof row.durationMs === "number" && Number.isFinite(row.durationMs)
            ? Math.max(0, Math.round(row.durationMs))
            : undefined,
        input: typeof row.input === "string" ? row.input : undefined,
        output: typeof row.output === "string" ? row.output : undefined,
      };
    })
    .slice(0, 8);

  if (count <= 0 && byName.length === 0 && samples.length === 0) return null;
  return {
    count,
    errorCount,
    byName,
    samples,
  };
}

function parsePmLiveToolEvent(
  detail?: Record<string, unknown>,
): PmLiveToolEvent | null {
  if (!detail || typeof detail !== "object") return null;
  const raw = (detail as Record<string, unknown>).liveToolEvent;
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const record = raw as Record<string, unknown>;
  const phaseRaw =
    typeof record.phase === "string" ? record.phase.toLowerCase() : "";
  if (phaseRaw !== "start" && phaseRaw !== "result" && phaseRaw !== "error") {
    return null;
  }
  const index =
    typeof record.index === "number" && Number.isFinite(record.index)
      ? Math.max(0, Math.round(record.index))
      : 0;
  const tool =
    typeof record.tool === "string" && record.tool.trim().length > 0
      ? record.tool
      : "unknown";
  const source = typeof record.source === "string" ? record.source : undefined;
  const targetFromRecord = extractPmTargetFromDetail(record);
  const target = targetFromRecord ?? undefined;
  const durationMs =
    typeof record.durationMs === "number" && Number.isFinite(record.durationMs)
      ? Math.max(0, Math.round(record.durationMs))
      : undefined;
  const isError = record.isError === true || phaseRaw === "error";
  return {
    phase: phaseRaw,
    index,
    tool,
    source,
    target,
    durationMs,
    isError,
  };
}

function extractDurationMs(detail?: Record<string, unknown>): number | null {
  if (!detail) return null;
  const raw = detail.durationMs;
  if (typeof raw === "number" && Number.isFinite(raw)) {
    return Math.max(0, Math.round(raw));
  }
  return null;
}

function fallbackStageNarrative(stage: string, status: PmStageStatus): string {
  if (status === "running") {
    if (stage === "preflight") return "正在执行启动健康检查";
    if (stage === "resume") return "正在恢复执行";
    if (stage === "understand") return "正在理解问题与目标";
    if (stage === "report_extract") return "正在提取报告关键信息";
    if (stage === "task_plan") return "正在生成研究计划";
    if (stage === "planner") return "正在拆解问题与检索路径";
    if (stage === "retrieve") return "正在跨来源检索与抓取证据";
    if (stage === "verify") return "正在做证据校验与冲突检查";
    if (stage === "retry_repair") return "正在自动修复证据缺口";
    if (stage === "synthesize") return "正在汇总结论与建议";
    return "正在执行";
  }
  if (status === "completed") {
    if (stage === "preflight") return "启动健康检查通过";
    if (stage === "resume") return "恢复执行已完成";
    if (stage === "understand") return "问题理解已完成";
    if (stage === "report_extract") return "报告关键信息提取已完成";
    if (stage === "task_plan") return "研究计划已生成";
    if (stage === "planner") return "检索计划已生成";
    if (stage === "retrieve") return "证据检索已完成";
    if (stage === "deep_loop") return "深度研究循环已完成";
    if (stage === "verify") return "证据校验已完成";
    if (stage === "retry_repair") return "自动修复已完成";
    if (stage === "synthesize") return "结论汇总已完成";
    return "已完成";
  }
  if (status === "failed") {
    if (stage === "preflight") return "启动健康检查失败";
    if (stage === "resume") return "恢复执行失败，继续尝试";
    if (stage === "report_extract") return "报告提取失败，继续使用本地解析";
    if (stage === "understand" || stage === "task_plan")
      return "预备阶段失败，继续尝试检索";
    if (stage === "retrieve") return "检索失败，准备自动修复";
    if (stage === "synthesize") return "总结完成，但质量未达标";
    return "执行失败";
  }
  return "等待执行";
}

function extractUrls(text: string): string[] {
  if (!text) return [];
  const matches = text.match(/https?:\/\/[^\s)\]]+/g) ?? [];
  const uniq = new Set<string>();
  for (const raw of matches) {
    uniq.add(raw.replace(/[.,;:!?]+$/, ""));
  }
  return Array.from(uniq).slice(0, 8);
}

function extractAllUrls(text: string): string[] {
  if (!text) return [];
  const matches = text.match(/https?:\/\/[^\s)\]]+/g) ?? [];
  const uniq = new Set<string>();
  for (const raw of matches) {
    uniq.add(raw.replace(/[.,;:!?]+$/, ""));
  }
  return Array.from(uniq);
}

function stripPmMarkdownLine(line: string): string {
  const trimmed = line.trim();
  if (!trimmed) return "";
  const withoutHeading = trimmed
    .replace(/^#{1,6}\s+/, "")
    .replace(/^\*\*(.+)\*\*$/, "$1")
    .replace(/^[-*•]\s+/, "")
    .replace(/^\d+\.\s+/, "")
    .trim();
  return withoutHeading;
}

function looksLikeMarkdownTableText(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed.includes("|")) return false;
  const pipeCount = (trimmed.match(/\|/g) ?? []).length;
  return (
    pipeCount >= 4 &&
    (/(\|\s*:?-{3,}:?\s*){2,}\|/.test(trimmed) || /\|\s+\|/.test(trimmed))
  );
}

function cleanPmDeliveryHighlight(line: string): string {
  const cleaned = stripPmMarkdownLine(line);
  if (looksLikeMarkdownTableText(cleaned)) {
    return cleaned;
  }
  return shortHumanText(cleaned, 180);
}

function extractPmDeliveryTitle(text: string, fallback: string): string {
  if (!text.trim()) return fallback;
  const lines = text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  for (const line of lines) {
    if (/^#{1,3}\s+/.test(line)) {
      return shortHumanText(stripPmMarkdownLine(line), 64);
    }
  }
  for (const line of lines) {
    if (
      /^[-*•]\s+/.test(line) ||
      /^\d+\.\s+/.test(line) ||
      /^https?:\/\//i.test(line)
    ) {
      continue;
    }
    const cleaned = stripPmMarkdownLine(line);
    if (looksLikeMarkdownTableText(cleaned)) {
      continue;
    }
    if (cleaned.length >= 8 && cleaned.length <= 44) {
      return shortHumanText(cleaned, 64);
    }
  }
  return fallback;
}

function extractPmDeliveryHighlights(text: string, maxItems = 4): string[] {
  if (!text.trim()) return [];
  const lines = text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const bullets = lines
    .filter((line) => /^[-*•]\s+/.test(line) || /^\d+\.\s+/.test(line))
    .map(cleanPmDeliveryHighlight)
    .filter((line) => line.length > 0);
  const bulletDedup = Array.from(new Set(bullets)).slice(0, maxItems);
  if (bulletDedup.length > 0) return bulletDedup;

  const tableBlocks = extractMarkdownTableBlocks(text, maxItems, 8);
  if (tableBlocks.length > 0) return tableBlocks;

  const plain = stripPmMarkdownLine(lines.join(" "));
  const sentences = plain
    .split(/(?<=[。！？.!?])\s+/)
    .map((line) => line.trim())
    .filter((line) => line.length >= 14)
    .map((line) => shortHumanText(line, 180));
  if (sentences.length > 0) {
    return Array.from(new Set(sentences)).slice(0, maxItems);
  }
  return plain ? [shortHumanText(plain, 180)] : [];
}

function isInternalPmContractText(text: string): boolean {
  const compact = text.trim();
  if (!compact) return false;
  if (
    /^(RETRIEVE|EXEC|PLAN|TASK)_[A-Z_]+_?CONSTRAINTS?\s*\{/i.test(compact) ||
    /^REPORT_JSON\s*\{/i.test(compact) ||
    /^SYNTHESIS_META\s*\{/i.test(compact) ||
    /^REPAIR_(SCOPE|RESULT)\s*\{/i.test(compact)
  ) {
    return true;
  }
  if (/^<EXEC_CONSTRAINTS>/i.test(compact)) return true;
  if (/^<RETRIEVE_CONSTRAINTS>/i.test(compact)) return true;
  return false;
}

function extractUrlDomain(url: string): string | null {
  try {
    return new URL(url).host.toLowerCase();
  } catch {
    return null;
  }
}

function normalizeEvidenceUrl(rawUrl: string): string | null {
  const trimmed = rawUrl.trim();
  if (!trimmed) return null;
  try {
    const parsed = new URL(trimmed);
    const protocol = parsed.protocol.toLowerCase();
    if (protocol !== "http:" && protocol !== "https:") {
      return null;
    }
    const host = parsed.host.toLowerCase();
    if (PM_PROVIDER_SOURCE_HOSTS.has(host)) {
      return null;
    }
    parsed.hash = "";
    return parsed.toString();
  } catch {
    return null;
  }
}

function sanitizeEvidenceUrls(
  urls: string[],
  options?: {
    limit?: number;
    dedupeByDomain?: boolean;
  },
): string[] {
  const limit = options?.limit ?? Number.POSITIVE_INFINITY;
  const dedupeByDomain = options?.dedupeByDomain === true;
  const seen = new Set<string>();
  const output: string[] = [];
  for (const rawUrl of urls) {
    const normalized = normalizeEvidenceUrl(rawUrl);
    if (!normalized) continue;
    const domain = extractUrlDomain(normalized);
    const dedupeKey = dedupeByDomain ? (domain ?? normalized) : normalized;
    if (seen.has(dedupeKey)) continue;
    seen.add(dedupeKey);
    output.push(normalized);
    if (output.length >= limit) break;
  }
  return output;
}

function estimatePmClaimCount(text: string): number {
  if (!text) return 0;
  let count = 0;
  for (const rawLine of text.split("\n")) {
    const line = rawLine.trim();
    if (line.length < 6) continue;
    const isBullet =
      line.startsWith("- ") || line.startsWith("* ") || line.startsWith("• ");
    const upper = line.toUpperCase();
    const hasLabel =
      upper.includes("FACT") ||
      upper.includes("HYPOTHESIS") ||
      upper.includes("RECOMMENDATION") ||
      line.includes("事实") ||
      line.includes("假设") ||
      line.includes("建议");
    if ((isBullet || hasLabel) && line.length >= 10) {
      count += 1;
    }
  }
  return count;
}

function extractPmClaimAlignmentRows(text: string): PmClaimEvidence[] {
  if (!text) return [];
  const rows: PmClaimEvidence[] = [];
  for (const rawLine of text.split("\n")) {
    const line = rawLine.trim();
    if (!line) continue;
    const isClaimLine =
      line.startsWith("- ") ||
      line.startsWith("* ") ||
      line.startsWith("• ") ||
      line.includes("FACT") ||
      line.includes("HYPOTHESIS") ||
      line.includes("RECOMMENDATION") ||
      line.includes("事实") ||
      line.includes("假设") ||
      line.includes("建议");
    if (!isClaimLine) continue;
    const urls = extractAllUrls(line);
    rows.push({
      claim: line.slice(0, 240),
      urls,
      cited: urls.length > 0,
    });
    if (rows.length >= 12) break;
  }
  return rows;
}

function buildPmQualitySnapshotFromHistory(
  answerText: string,
  toolCalls: ToolCallInfo[] | undefined,
): PmQualitySnapshot {
  const citations = extractAllUrls(answerText);
  const domainsSet = new Set<string>();
  for (const url of citations) {
    const host = extractUrlDomain(url);
    if (host) domainsSet.add(host);
  }
  const domains = Array.from(domainsSet);
  const claimAlignment = extractPmClaimAlignmentRows(answerText);
  const claimCount = estimatePmClaimCount(answerText);
  const citedClaimCount = claimAlignment.filter((row) => row.cited).length;
  const claimAlignmentOk =
    claimCount === 0
      ? citations.length > 0
      : citedClaimCount * 100 >= claimCount * 60;
  const hasToolCalls = (toolCalls?.length ?? 0) > 0;
  const missing: string[] = [];
  const suggestions: string[] = [];
  if (!hasToolCalls) {
    missing.push("missing_tool_retrieval");
    suggestions.push(
      "Enable search/browser MCP tools and let assistant retrieve evidence first.",
    );
  }
  if (citations.length === 0) {
    missing.push("missing_citations");
    suggestions.push(
      "Provide source URLs for each key fact and mark uncertain items explicitly.",
    );
  }
  if (citations.length > 0 && citations.length < 3) {
    missing.push("insufficient_citations");
    suggestions.push("Increase citation coverage to at least 3 URLs.");
  }
  if (domains.length < 2) {
    missing.push("insufficient_domain_diversity");
    suggestions.push(
      "Use at least 2 distinct source domains and cross-check consistency.",
    );
  }
  if (!claimAlignmentOk) {
    missing.push("low_claim_evidence_alignment");
    suggestions.push(
      "Align each claim with evidence: add URLs near each key finding.",
    );
  }
  return {
    passed:
      hasToolCalls &&
      citations.length >= 3 &&
      domains.length >= 2 &&
      claimAlignmentOk,
    has_tool_calls: hasToolCalls,
    tool_call_count: toolCalls?.length ?? 0,
    citation_count: citations.length,
    domain_count: domains.length,
    claim_count: claimCount,
    claim_alignment_ok: claimAlignmentOk,
    citations,
    domains,
    claim_alignment: claimAlignment,
    evidence_tree: claimAlignment.map((row) => ({
      claim: row.claim,
      status: row.cited ? "confirmed" : "gap",
      evidence_count: row.urls.length,
      evidences:
        row.urls.length > 0
          ? row.urls.slice(0, 4).map((url) => ({
              url,
              domain: extractUrlDomain(url) ?? "",
              excerpt: row.evidence_excerpt ?? row.claim,
            }))
          : [
              {
                url: "",
                domain: "",
                excerpt: row.evidence_excerpt ?? row.claim,
              },
            ],
    })),
    conflict_matrix: [],
    conflict_graph: {
      topic_count: 0,
      edge_count: 0,
      adjudicated_count: 0,
      unresolved_count: 0,
      avg_confidence: 0,
      edges: [],
    },
    missing,
    suggestions,
  };
}

function formatMarkdownTimestamp(date: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

function coerceDisplayTimestampMs(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value > 0 && value < 10_000_000_000 ? value * 1000 : value;
  }
  if (typeof value === "string" && value.trim().length > 0) {
    const numeric = Number(value);
    if (Number.isFinite(numeric)) {
      return numeric > 0 && numeric < 10_000_000_000 ? numeric * 1000 : numeric;
    }
    const parsed = Date.parse(value);
    return Number.isFinite(parsed) ? parsed : undefined;
  }
  return undefined;
}

function pickMessageTimestampMs(
  value: unknown,
  fallbackMs?: number,
): number | undefined {
  if (!value || typeof value !== "object") return fallbackMs;
  const obj = value as Record<string, unknown>;
  return (
    coerceDisplayTimestampMs(obj.timestamp) ??
    coerceDisplayTimestampMs(obj.createdAt) ??
    coerceDisplayTimestampMs(obj.created_at) ??
    coerceDisplayTimestampMs(obj.createdAtMs) ??
    coerceDisplayTimestampMs(obj.created_at_ms) ??
    coerceDisplayTimestampMs(obj.timestampMs) ??
    fallbackMs
  );
}

function formatDisplayMessageTimestamp(message: DisplayMessage): string | null {
  const timestamp = pickMessageTimestampMs(message);
  if (timestamp == null) return null;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return null;
  return formatMarkdownTimestamp(date);
}

function buildSessionMarkdown(
  sessionId: string | null,
  source: "chat" | "agent" | "pm",
  messages: DisplayMessage[],
  pmQuality: PmQualitySnapshot | null,
): string {
  const lines: string[] = [];
  lines.push("# AOS Conversation Export");
  lines.push("");
  lines.push(`- Session: ${sessionId ?? "new"}`);
  lines.push(`- Source: ${source}`);
  lines.push(`- Exported At: ${formatMarkdownTimestamp(new Date())}`);
  lines.push("");
  for (let i = 0; i < messages.length; i += 1) {
    const msg = messages[i];
    const role = msg.role === "user" ? "User" : "Assistant";
    const messageTime = formatDisplayMessageTimestamp(msg);
    lines.push(`## ${i + 1}. ${role}${messageTime ? ` · ${messageTime}` : ""}`);
    lines.push("");
    const content = contentToPlain(msg.content).trim();
    lines.push(content || "_[empty]_");
    lines.push("");
    if (msg.toolCalls && msg.toolCalls.length > 0) {
      lines.push("### Tool Calls");
      for (const tc of msg.toolCalls) {
        const sourceLabel =
          tc.source === "mcp"
            ? `mcp:${tc.mcpServer ?? "unknown"}`
            : tc.source === "skill"
              ? `skill:${tc.skillName ?? "unknown"}`
              : "builtin";
        lines.push(
          `- \`${tc.name}\` | ${sourceLabel} | ${tc.isError ? "error" : "success"}`,
        );
      }
      lines.push("");
    }
  }
  if (source === "pm" && pmQuality) {
    lines.push("## PM Quality");
    lines.push("");
    lines.push(`- Passed: ${pmQuality.passed ? "yes" : "no"}`);
    lines.push(`- Tool Calls: ${pmQuality.tool_call_count}`);
    lines.push(`- Citations: ${pmQuality.citation_count}`);
    lines.push(`- Domains: ${pmQuality.domain_count ?? 0}`);
    lines.push(`- Claim Count: ${pmQuality.claim_count ?? 0}`);
    lines.push(
      `- Claim Alignment: ${pmQuality.claim_alignment_ok ? "ok" : "weak"}`,
    );
    lines.push("");
    if ((pmQuality.claim_alignment?.length ?? 0) > 0) {
      lines.push("### Claim-Evidence Alignment");
      for (const row of pmQuality.claim_alignment ?? []) {
        lines.push(`- [${row.cited ? "x" : " "}] ${row.claim}`);
        if (row.urls.length > 0) {
          for (const url of row.urls.slice(0, 6)) {
            lines.push(`  - ${url}`);
          }
        }
      }
      lines.push("");
    }
  }
  return lines.join("\n").trimEnd() + "\n";
}

function buildRouteKey(route: string, channel?: string): string {
  return channel ? `${route}::${channel}` : route;
}

function pickRetrieveRouteInfo(detail?: Record<string, unknown>): {
  route: string;
  channel?: string;
  variant?: string;
  durationMs?: number;
} {
  const selectedRoute =
    typeof detail?.selectedRoute === "string" &&
    detail.selectedRoute.trim().length > 0
      ? detail.selectedRoute.trim()
      : typeof detail?.route === "string" && detail.route.trim().length > 0
        ? detail.route.trim()
        : "auto_route";
  const selectedChannel =
    typeof detail?.selectedRouteChannel === "string" &&
    detail.selectedRouteChannel.trim().length > 0
      ? detail.selectedRouteChannel.trim()
      : undefined;
  const selectedVariant =
    typeof detail?.selectedVariant === "string" &&
    detail.selectedVariant.trim().length > 0
      ? detail.selectedVariant.trim()
      : undefined;
  const durationMs =
    typeof detail?.durationMs === "number" && Number.isFinite(detail.durationMs)
      ? Math.max(0, Math.round(detail.durationMs))
      : undefined;
  return {
    route: selectedRoute,
    channel: selectedChannel,
    variant: selectedVariant,
    durationMs,
  };
}

function extractClaimEvidenceExcerpt(answer: string, claim: string): string {
  const full = answer?.trim();
  if (!full) return "";
  const normalizedClaim = claim.trim();
  if (!normalizedClaim) return full.slice(0, 280);

  const haystack = full.toLowerCase();
  let idx = haystack.indexOf(normalizedClaim.toLowerCase());
  if (idx < 0) {
    const tokens = normalizedClaim
      .split(/[\s,，。;；:：()（）[\]【】/\\'"`]+/)
      .map((item) => item.trim())
      .filter((item) => item.length >= 3)
      .sort((a, b) => b.length - a.length);
    for (const token of tokens) {
      const next = haystack.indexOf(token.toLowerCase());
      if (next >= 0) {
        idx = next;
        break;
      }
    }
  }

  if (idx < 0) {
    return full.slice(0, 280);
  }
  const start = Math.max(0, idx - 120);
  const end = Math.min(
    full.length,
    idx + Math.max(220, normalizedClaim.length + 120),
  );
  const prefix = start > 0 ? "..." : "";
  const suffix = end < full.length ? "..." : "";
  return `${prefix}${full.slice(start, end)}${suffix}`;
}

function normalizePmReportArtifact(
  value: unknown,
): PmReportArtifact | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return undefined;
  const obj = value as Record<string, unknown>;
  const schemaVersion =
    typeof obj.schemaVersion === "string"
      ? obj.schemaVersion
      : typeof obj.schema_version === "string"
        ? (obj.schema_version as string)
        : undefined;
  const questionType =
    typeof obj.questionType === "string"
      ? obj.questionType
      : typeof obj.question_type === "string"
        ? (obj.question_type as string)
        : undefined;
  const quantEnabled =
    typeof obj.quantEnabled === "boolean"
      ? obj.quantEnabled
      : typeof obj.quant_enabled === "boolean"
        ? (obj.quant_enabled as boolean)
        : undefined;
  const reportJson =
    obj.reportJson &&
    typeof obj.reportJson === "object" &&
    !Array.isArray(obj.reportJson)
      ? (obj.reportJson as Record<string, unknown>)
      : obj.report_json &&
          typeof obj.report_json === "object" &&
          !Array.isArray(obj.report_json)
        ? (obj.report_json as Record<string, unknown>)
        : undefined;
  const reportHtml =
    typeof obj.reportHtml === "string"
      ? obj.reportHtml
      : typeof obj.report_html === "string"
        ? (obj.report_html as string)
        : undefined;
  const reportJsonV3 =
    obj.reportJsonV3 &&
    typeof obj.reportJsonV3 === "object" &&
    !Array.isArray(obj.reportJsonV3)
      ? (obj.reportJsonV3 as Record<string, unknown>)
      : obj.report_json_v3 &&
          typeof obj.report_json_v3 === "object" &&
          !Array.isArray(obj.report_json_v3)
        ? (obj.report_json_v3 as Record<string, unknown>)
        : undefined;
  const reportHtmlV3 =
    typeof obj.reportHtmlV3 === "string"
      ? obj.reportHtmlV3
      : typeof obj.report_html_v3 === "string"
        ? (obj.report_html_v3 as string)
        : undefined;
  if (
    !schemaVersion &&
    !questionType &&
    !reportJson &&
    !reportHtml &&
    !reportJsonV3 &&
    !reportHtmlV3
  ) {
    return undefined;
  }
  return {
    schemaVersion,
    questionType,
    quantEnabled,
    reportJson,
    reportHtml,
    reportJsonV3,
    reportHtmlV3,
  };
}

function normalizePmQualitySnapshot(
  value: unknown,
): PmQualitySnapshot | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return undefined;
  const obj = value as Record<string, unknown>;
  if (typeof obj.passed !== "boolean") return undefined;

  const citations = Array.isArray(obj.citations)
    ? obj.citations.filter((item): item is string => typeof item === "string")
    : undefined;
  const domains = Array.isArray(obj.domains)
    ? obj.domains.filter((item): item is string => typeof item === "string")
    : undefined;
  const missing = Array.isArray(obj.missing)
    ? obj.missing.filter((item): item is string => typeof item === "string")
    : [];
  const suggestions = Array.isArray(obj.suggestions)
    ? obj.suggestions.filter((item): item is string => typeof item === "string")
    : [];

  return {
    passed: obj.passed,
    deliverable:
      typeof obj.deliverable === "boolean" ? obj.deliverable : undefined,
    quality_level:
      typeof obj.quality_level === "string" ? obj.quality_level : undefined,
    has_tool_calls:
      typeof obj.has_tool_calls === "boolean" ? obj.has_tool_calls : false,
    tool_call_count:
      typeof obj.tool_call_count === "number" ? obj.tool_call_count : 0,
    citation_count:
      typeof obj.citation_count === "number"
        ? obj.citation_count
        : (citations?.length ?? 0),
    domain_count:
      typeof obj.domain_count === "number" ? obj.domain_count : domains?.length,
    claim_count:
      typeof obj.claim_count === "number" ? obj.claim_count : undefined,
    claim_alignment_ok:
      typeof obj.claim_alignment_ok === "boolean"
        ? obj.claim_alignment_ok
        : undefined,
    citations,
    domains,
    claim_alignment: Array.isArray(obj.claim_alignment)
      ? (obj.claim_alignment as PmClaimEvidence[])
      : undefined,
    evidence_tree: Array.isArray(obj.evidence_tree)
      ? (obj.evidence_tree as PmEvidenceTreeNode[])
      : undefined,
    conflict_matrix: Array.isArray(obj.conflict_matrix)
      ? (obj.conflict_matrix as PmConflictRow[])
      : undefined,
    conflict_graph:
      obj.conflict_graph &&
      typeof obj.conflict_graph === "object" &&
      !Array.isArray(obj.conflict_graph)
        ? (obj.conflict_graph as PmConflictGraph)
        : undefined,
    missing,
    suggestions,
  };
}

function resolvePmTerminalMessageText(
  event: ApiPmResearchTaskEvent,
  fallbackUnknownText: string,
): string {
  const responseAny = event.response as
    | {
        text?: string;
      }
    | undefined;
  const detail =
    event.detail &&
    typeof event.detail === "object" &&
    !Array.isArray(event.detail)
      ? (event.detail as Record<string, unknown>)
      : undefined;
  const directText =
    typeof responseAny?.text === "string" ? responseAny.text.trim() : "";
  const detailReason =
    pickPmDetailString(detail, "error") ||
    pickPmDetailString(detail, "reason") ||
    pickPmDetailString(detail, "message") ||
    pickPmDetailString(detail, "failureReason");
  const detailPreview = pickPmDetailString(detail, "preview");
  const fallbackReason =
    (event.error && event.error.trim()) ||
    (event.message && event.message.trim()) ||
    detailReason ||
    fallbackUnknownText;
  const fallbackText =
    event.status === "failed"
      ? detailPreview
        ? `研究任务失败：${fallbackReason}\n\n（以下为失败前生成的草稿，不代表最终结论）\n${detailPreview}`
        : `研究任务失败：${fallbackReason}`
      : event.status === "cancelled"
        ? "研究任务已取消。"
        : "研究任务已完成，但未返回可展示的结论文本。";
  return (directText || fallbackText).trim();
}

function historyContentToPlain(rawContent: unknown): string {
  if (typeof rawContent === "string") return rawContent;
  if (Array.isArray(rawContent)) {
    return contentToPlain(rawContent as ContentBlock[]);
  }
  if (rawContent == null) return "";
  if (typeof rawContent === "object") {
    const text = (rawContent as { text?: unknown }).text;
    if (typeof text === "string") return text;
    return contentToPlain([rawContent as ContentBlock]);
  }
  return String(rawContent);
}

function mergeHistoryThinking(
  existing: string | null | undefined,
  incoming: string | null | undefined,
): string | null {
  const next = (incoming ?? "").trim();
  if (!next) return existing?.trim() || null;
  const prev = (existing ?? "").trim();
  if (!prev) return next;
  if (prev === next || prev.includes(next)) return prev;
  if (next.includes(prev)) return next;
  return `${prev}\n\n${next}`;
}

function historyToolCallKey(tool: ToolCallInfo): string {
  return [
    tool.source,
    tool.mcpServer ?? "",
    tool.skillName ?? "",
    tool.name,
    tool.args ?? "",
    tool.result ?? "",
    tool.status,
    tool.isError ? "1" : "0",
  ].join("\u0001");
}

function mergeHistoryToolCalls(
  existing?: ToolCallInfo[],
  incoming?: ToolCallInfo[],
): ToolCallInfo[] {
  const out = [...(existing ?? [])];
  const seen = new Set(out.map(historyToolCallKey));
  for (const tool of incoming ?? []) {
    const key = historyToolCallKey(tool);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(tool);
  }
  return out;
}

function mergeHistoryEvidenceSources(
  existing?: ChatEvidenceSource[],
  incoming?: ChatEvidenceSource[],
): ChatEvidenceSource[] | undefined {
  const out = [...(existing ?? [])];
  const seen = new Set(
    out.map((source) =>
      [
        source.id ?? "",
        source.type,
        source.url ?? "",
        source.fileId ?? "",
        source.memoryId ?? "",
        source.title ?? "",
      ].join("\u0001"),
    ),
  );
  for (const source of incoming ?? []) {
    const key = [
      source.id ?? "",
      source.type,
      source.url ?? "",
      source.fileId ?? "",
      source.memoryId ?? "",
      source.title ?? "",
    ].join("\u0001");
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(source);
  }
  return out.length > 0 ? out : undefined;
}

function withAssistantArtifacts(
  message: DisplayMessage,
  toolCalls?: ToolCallInfo[],
  thinking?: string | null,
): DisplayMessage {
  const nextToolCalls = mergeHistoryToolCalls(message.toolCalls, toolCalls);
  const nextThinking = mergeHistoryThinking(message.thinking, thinking);
  return {
    ...message,
    toolCalls: nextToolCalls.length > 0 ? nextToolCalls : undefined,
    thinking: nextThinking,
  };
}

function mergeDisplayMessageSearchUsage(
  base?: PmSearchUsageSummary,
  incoming?: PmSearchUsageSummary,
): PmSearchUsageSummary | undefined {
  return mergePmSearchUsageSummaries(base, incoming) ?? undefined;
}

function mergeHistoryAssistantMessages(
  base: DisplayMessage,
  incoming: DisplayMessage,
): DisplayMessage {
  return {
    ...incoming,
    timestamp: incoming.timestamp ?? base.timestamp,
    createdAt: incoming.createdAt ?? base.createdAt,
    toolCalls: mergeHistoryToolCalls(base.toolCalls, incoming.toolCalls),
    thinking: mergeHistoryThinking(base.thinking, incoming.thinking),
    evidenceSources: mergeHistoryEvidenceSources(
      base.evidenceSources,
      incoming.evidenceSources,
    ),
    pmTaskId: incoming.pmTaskId ?? base.pmTaskId,
    pmTaskStatus: incoming.pmTaskStatus ?? base.pmTaskStatus,
    pmReport: incoming.pmReport ?? base.pmReport,
    pmFinalDelivery: incoming.pmFinalDelivery ?? base.pmFinalDelivery,
    attributionTaskId: incoming.attributionTaskId ?? base.attributionTaskId,
    superAssistantTurnId:
      incoming.superAssistantTurnId ?? base.superAssistantTurnId,
    pmSearchUsage: mergeDisplayMessageSearchUsage(
      base.pmSearchUsage,
      incoming.pmSearchUsage,
    ),
    traceEvents: [
      ...(base.traceEvents ?? []),
      ...(incoming.traceEvents ?? []),
    ].slice(-40),
  };
}

function isHistoryProgressOnlyAssistant(message: DisplayMessage): boolean {
  if (message.role !== "assistant") return false;
  const plain = historyContentToPlain(message.content).trim();
  if (!plain) return true;
  return isHistoricalToolBridgeText(plain);
}

function evidenceSourcesFromDocuments(
  documents: DocumentBlock[],
): ChatEvidenceSource[] {
  return documents.map((document, index) => {
    const filename =
      document.name ||
      document.data.split("/").pop()?.split("?")[0] ||
      document.fileId ||
      `file-${index + 1}`;
    return {
      id: document.fileId || `${document.data}-${index}`,
      type: "file",
      title: `[file:${index + 1}] ${filename}`,
      url: document.data,
      fileId: document.fileId,
      filename,
    };
  });
}

function evidenceSourcesFromTurn(
  text: string,
  toolCalls: ToolCallInfo[],
  documents: DocumentBlock[],
): ChatEvidenceSource[] {
  const sources = evidenceSourcesFromDocuments(documents);
  const seenUrls = new Set<string>();
  const pushUrl = (rawUrl: string) => {
    const trimmed = rawUrl.trim().replace(/[.,;:!?]+$/, "");
    if (!/^https?:\/\//i.test(trimmed) || seenUrls.has(trimmed)) return;
    seenUrls.add(trimmed);
    let title = trimmed;
    try {
      const parsed = new URL(trimmed);
      title = parsed.hostname || trimmed;
    } catch {
      // keep raw URL
    }
    sources.push({
      id: `web-${sources.length}-${trimmed}`,
      type: "web",
      title,
      url: trimmed,
    });
  };
  for (const url of text.match(/https?:\/\/[^\s)\]]+/g) ?? []) {
    pushUrl(url);
  }
  for (const tool of toolCalls) {
    const combined = `${tool.args ?? ""}\n${tool.result ?? ""}`;
    for (const url of combined.match(/https?:\/\/[^\s)\]]+/g) ?? []) {
      pushUrl(url);
    }
  }
  return sources;
}

function memoryCitationToEvidenceSource(
  citation: AgentMemoryCitation,
  index: number,
): ChatEvidenceSource {
  const path = citation.path?.trim() || `MEMORY.md`;
  const lineSuffix =
    citation.lineStart != null
      ? citation.lineEnd != null && citation.lineEnd !== citation.lineStart
        ? `:${citation.lineStart}-${citation.lineEnd}`
        : `:${citation.lineStart}`
      : "";
  return {
    id: citation.id || `memory-citation-${index}`,
    type: "memory",
    title: citation.note?.trim() || path,
    memoryId: citation.memoryId,
    sessionId: citation.turnId ?? undefined,
    lineStart: citation.lineStart ?? undefined,
    lineEnd: citation.lineEnd ?? undefined,
    snippet: citation.note ?? undefined,
    sourceLabel: `memory · ${path}${lineSuffix}`,
  };
}

function attachMemoryCitationsToMessages(
  messages: DisplayMessage[],
  citations: AgentMemoryCitation[],
): DisplayMessage[] {
  if (messages.length === 0 || citations.length === 0) return messages;
  const out = messages.map((msg) => ({ ...msg }));
  const assistantIndexes = out
    .map((msg, index) => (msg.role === "assistant" ? index : -1))
    .filter((index): index is number => index >= 0);
  if (assistantIndexes.length === 0) return messages;

  const byTurnId = new Map<string, AgentMemoryCitation[]>();
  const fallback: AgentMemoryCitation[] = [];
  for (const citation of citations) {
    const turnId = citation.turnId?.trim();
    if (turnId) {
      const bucket = byTurnId.get(turnId) ?? [];
      bucket.push(citation);
      byTurnId.set(turnId, bucket);
    } else {
      fallback.push(citation);
    }
  }

  const appendSources = (index: number, items: AgentMemoryCitation[]) => {
    if (index < 0 || index >= out.length || items.length === 0) return;
    const existing = out[index].evidenceSources ?? [];
    const seen = new Set(
      existing.map(
        (source) =>
          source.id || source.memoryId || `${source.type}:${source.title}`,
      ),
    );
    const nextSources = [...existing];
    items.forEach((citation, citationIndex) => {
      const source = memoryCitationToEvidenceSource(citation, citationIndex);
      const key =
        source.id || source.memoryId || `${source.type}:${source.title}`;
      if (seen.has(key)) return;
      seen.add(key);
      nextSources.push(source);
    });
    out[index] = {
      ...out[index],
      evidenceSources: nextSources,
    };
  };

  const consumeByAssistantOrder = (() => {
    let pointer = 0;
    return (items: AgentMemoryCitation[]) => {
      if (items.length === 0) return;
      const index =
        assistantIndexes[Math.min(pointer, assistantIndexes.length - 1)];
      appendSources(index, items);
      pointer = Math.min(pointer + 1, assistantIndexes.length - 1);
    };
  })();

  for (const [turnId, items] of byTurnId.entries()) {
    const matchedIndex = out.findIndex((msg) => msg.id === turnId);
    if (matchedIndex >= 0) {
      appendSources(matchedIndex, items);
      continue;
    }
    consumeByAssistantOrder(items);
  }

  if (fallback.length > 0) {
    consumeByAssistantOrder(fallback);
  }

  return out;
}

function attachSessionMemoryCitations(
  messages: DisplayMessage[],
  citations: AgentMemoryCitation[],
  options?: { sinceMs?: number },
): DisplayMessage[] {
  if (messages.length === 0 || citations.length === 0) return messages;
  const normalized = [...messages];
  const groupedByTurn = new Map<string, AgentMemoryCitation[]>();
  const sessionFallback: AgentMemoryCitation[] = [];
  const filteredCitations =
    options?.sinceMs != null
      ? citations.filter((citation) => {
          const createdMs = Date.parse(citation.createdAt);
          return Number.isFinite(createdMs) && createdMs >= options.sinceMs!;
        })
      : citations;

  for (const citation of filteredCitations) {
    const turnId = citation.turnId?.trim();
    if (turnId) {
      const bucket = groupedByTurn.get(turnId) ?? [];
      bucket.push(citation);
      groupedByTurn.set(turnId, bucket);
    } else {
      sessionFallback.push(citation);
    }
  }

  let fallbackCursor = 0;
  const lastAssistantIndexes: number[] = [];
  normalized.forEach((msg, idx) => {
    if (msg.role === "assistant") lastAssistantIndexes.push(idx);
  });

  const appendSources = (idx: number, items: AgentMemoryCitation[]) => {
    if (items.length === 0 || idx < 0 || idx >= normalized.length) return;
    const current = normalized[idx];
    const sources = [...(current.evidenceSources ?? [])];
    const seen = new Set(
      sources.map(
        (source) =>
          source.id || source.memoryId || `${source.type}:${source.title}`,
      ),
    );
    for (const [citationIndex, citation] of items.entries()) {
      const source = memoryCitationToEvidenceSource(citation, citationIndex);
      const key =
        source.id || source.memoryId || `${source.type}:${source.title}`;
      if (seen.has(key)) continue;
      seen.add(key);
      sources.push(source);
    }
    normalized[idx] = {
      ...current,
      evidenceSources: sources,
    };
  };

  for (const [turnId, items] of groupedByTurn.entries()) {
    const messageIndex = normalized.findIndex((msg) => msg.id === turnId);
    if (messageIndex >= 0) {
      appendSources(messageIndex, items);
    } else if (items.length > 0 && lastAssistantIndexes.length > 0) {
      const idx =
        lastAssistantIndexes[
          Math.min(fallbackCursor, lastAssistantIndexes.length - 1)
        ];
      appendSources(idx, items);
      fallbackCursor = Math.min(
        fallbackCursor + 1,
        lastAssistantIndexes.length - 1,
      );
    }
  }

  if (sessionFallback.length > 0 && lastAssistantIndexes.length > 0) {
    const idx = lastAssistantIndexes[lastAssistantIndexes.length - 1];
    appendSources(idx, sessionFallback);
  }

  return normalized;
}

function buildPmSearchUsageFromEvents(
  events: Array<{ detail?: Record<string, unknown> | null }>,
): PmSearchUsageSummary | undefined {
  const merged = events.reduce<PmSearchUsageSummary | null>((acc, event) => {
    const detail =
      event.detail &&
      typeof event.detail === "object" &&
      !Array.isArray(event.detail)
        ? event.detail
        : undefined;
    return mergePmSearchUsageSummaries(
      acc,
      parsePmSearchUsageSummary(detail, parsePmToolSummary(detail)),
    );
  }, null);
  return merged ?? undefined;
}

function pmStageEventsToMessageArtifacts(events: PmStageEvent[]): {
  pmSearchUsage?: PmSearchUsageSummary;
  traceEvents?: Record<string, unknown>[];
} {
  const pmSearchUsage = buildPmSearchUsageFromEvents(
    events.map((stageEvent) => ({ detail: stageEvent.detail })),
  );
  const traceEvents = events
    .map((stageEvent) => ({
      stage: stageEvent.stage,
      status: stageEvent.status,
      attempt: stageEvent.attempt,
      detail: stageEvent.detail,
    }))
    .slice(-40);
  return {
    pmSearchUsage,
    traceEvents: traceEvents.length > 0 ? traceEvents : undefined,
  };
}

function attachPmSearchUsageToLatestAssistant(
  messages: DisplayMessage[],
  searchUsage?: PmSearchUsageSummary,
  traceEvents?: Record<string, unknown>[],
): DisplayMessage[] {
  if (!searchUsage && (!traceEvents || traceEvents.length === 0))
    return messages;
  const out = [...messages];
  for (let i = out.length - 1; i >= 0; i -= 1) {
    if (out[i].role !== "assistant") continue;
    out[i] = {
      ...out[i],
      pmSearchUsage: mergeDisplayMessageSearchUsage(
        out[i].pmSearchUsage,
        searchUsage,
      ),
      traceEvents: [
        ...(out[i].traceEvents ?? []),
        ...(traceEvents ?? []),
      ].slice(-40),
    };
    break;
  }
  return out;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function chatArtifactEvidenceToSource(
  item: ChatArtifactEvidenceItem,
  index: number,
): ChatEvidenceSource {
  const rawType = (item.type ?? "").toLowerCase();
  const type: ChatEvidenceSource["type"] =
    rawType === "file" ? "file" : rawType === "memory" ? "memory" : "web";
  return {
    id:
      item.url ||
      item.fileId ||
      item.memoryId ||
      item.path ||
      `chat-artifact-source-${index}`,
    type,
    title:
      item.title ||
      item.filename ||
      item.path ||
      item.url ||
      `source-${index + 1}`,
    url: item.url,
    fileId: item.fileId,
    filename: item.filename,
    memoryId: item.memoryId,
    sessionId: item.sessionId,
    lineStart: item.lineStart ?? undefined,
    lineEnd: item.lineEnd ?? undefined,
    snippet: item.snippet,
    sourceLabel: item.path,
  };
}

function chatFileCitationToSource(
  item: Record<string, unknown>,
  index: number,
): ChatEvidenceSource {
  const fileId = typeof item.fileId === "string" ? item.fileId : undefined;
  const filename =
    typeof item.filename === "string" && item.filename.trim().length > 0
      ? item.filename.trim()
      : `file-${index + 1}`;
  const citation =
    typeof item.citation === "string" && item.citation.trim().length > 0
      ? item.citation.trim()
      : filename;
  const excerpt =
    typeof item.excerpt === "string" && item.excerpt.trim().length > 0
      ? item.excerpt.trim()
      : undefined;
  return {
    id: fileId || `chat-file-artifact-${index}-${citation}`,
    type: "file",
    title: citation,
    fileId,
    filename,
    snippet: excerpt,
    sourceLabel: filename,
  };
}

function flattenChatEvidenceArtifacts(
  artifacts: unknown[],
): ChatEvidenceSource[] {
  const sources: ChatEvidenceSource[] = [];
  const pushSource = (source: ChatEvidenceSource | undefined) => {
    if (!source) return;
    sources.push(source);
  };

  artifacts.forEach((artifact, artifactIndex) => {
    if (!isRecord(artifact)) return;
    const artifactType = String(artifact.type ?? "").toLowerCase();
    const items = Array.isArray(artifact.items) ? artifact.items : [];
    if (items.length > 0) {
      items.forEach((item, itemIndex) => {
        if (!isRecord(item)) return;
        if (artifactType === "file") {
          pushSource(
            chatFileCitationToSource(item, sources.length + itemIndex),
          );
          return;
        }
        pushSource(
          chatArtifactEvidenceToSource(
            item as ChatArtifactEvidenceItem,
            sources.length + itemIndex,
          ),
        );
      });
      return;
    }

    if (artifactType === "memory") {
      const metadata = isRecord(artifact.metadata)
        ? artifact.metadata
        : undefined;
      pushSource({
        id:
          (typeof metadata?.id === "string" && metadata.id) ||
          (typeof metadata?.memoryId === "string" && metadata.memoryId) ||
          `chat-memory-artifact-${artifactIndex}`,
        type: "memory",
        title:
          (typeof artifact.title === "string" && artifact.title) ||
          "Long-term memory",
        memoryId:
          (typeof metadata?.memoryId === "string" && metadata.memoryId) ||
          (typeof metadata?.id === "string" && metadata.id) ||
          undefined,
        snippet:
          (typeof metadata?.summary === "string" && metadata.summary) ||
          (typeof metadata?.content === "string" && metadata.content) ||
          undefined,
        sourceLabel:
          typeof metadata?.path === "string" ? metadata.path : "memory",
      });
      return;
    }

    pushSource(
      chatArtifactEvidenceToSource(
        artifact as ChatArtifactEvidenceItem,
        artifactIndex,
      ),
    );
  });

  return sources;
}

function flattenChatTraceArtifacts(
  artifacts: Array<Record<string, unknown>>,
): Array<Record<string, unknown>> {
  const events: Array<Record<string, unknown>> = [];
  for (const artifact of artifacts) {
    if (!isRecord(artifact)) continue;
    const items = Array.isArray(artifact.items) ? artifact.items : undefined;
    if (items) {
      for (const item of items) {
        if (isRecord(item)) events.push(item);
      }
      continue;
    }
    events.push(artifact);
  }
  return events;
}

function latestChatArtifactPayloads<T>(items: T[] | undefined): T[] {
  if (!items || items.length === 0) return [];
  return [items[0]];
}

function attachChatArtifactsToLatestAssistant(
  messages: DisplayMessage[],
  evidence: unknown[],
  trace: Array<Record<string, unknown>>,
  targetMessageId?: string,
): DisplayMessage[] {
  if (evidence.length === 0 && trace.length === 0) return messages;
  const out = [...messages];
  const targetIndex =
    targetMessageId && targetMessageId.trim().length > 0
      ? out.findIndex(
          (msg) => msg.id === targetMessageId && msg.role === "assistant",
        )
      : -1;
  if (targetMessageId && targetIndex < 0) return messages;
  const startIndex = targetIndex >= 0 ? targetIndex : out.length - 1;
  for (let i = startIndex; i >= 0; i -= 1) {
    if (out[i].role !== "assistant") {
      if (targetIndex >= 0) break;
      continue;
    }
    const artifactSources = flattenChatEvidenceArtifacts(evidence);
    const traceEvents = flattenChatTraceArtifacts(trace);
    out[i] = {
      ...out[i],
      evidenceSources: mergeHistoryEvidenceSources(
        out[i].evidenceSources,
        artifactSources,
      ),
      traceEvents: [...(out[i].traceEvents ?? []), ...traceEvents].slice(-40),
    };
    break;
  }
  return out;
}

function extractHistoricalThinkingText(text: string): string | null {
  const compact = text.trim();
  if (!compact) return null;
  const match =
    compact.match(/^(?:Thought|Thinking|Reasoning)\s*[·:：-]\s*([\s\S]+)$/i) ??
    compact.match(/^(?:思考中|思考|思路|已深度思考)\s*[·:：-]\s*([\s\S]+)$/);
  const thinking = match?.[1]?.trim();
  return thinking && thinking.length > 0 ? thinking : null;
}

function isHistoricalToolBridgeText(text: string): boolean {
  const compact = text.trim().replace(/\s+/g, " ");
  if (!compact || compact.length > 1600) return false;

  const patterns = [
    /^Completed\s+\d+\s+steps?\s+and\s+formed\s+an\s+evidence\s+chain\b/i,
    /^Processing\s+\d+\s+steps?\b/i,
    /^\d+\s+steps?\s+(?:failed|execution failed)\b/i,
    /^Latest progress\s*[:：]/i,
    /^Sources?\s+\d+\s+(?:sites?|domains?)\b/i,
    /^已完成\s+\d+\s*个步骤[，,]\s*围绕[\s\S]{0,180}形成证据链/,
    /^正在处理\s+\d+\s*个步骤/,
    /^\d+\s*个步骤执行失败/,
    /^最新进展\s*[:：]/,
    /^来源\s+\d+\s*个站点\s*[:：]/,
    /^(问题理解|证据检索|证据校验|自动修复|结论汇总|理解与规划|启动健康检查|任务规划|模型预检|检索预检).{0,100}(已完成|进行中|正在|失败|超时)/,
    /^(正在|已完成)(搜索|提取|校验|分析|整理|执行)\s*[:：]/,
    /^(Search|Extract|Verify|Analyze|Synthesize|Execute)\s+(completed|failed)\s*:/i,
  ];

  return patterns.some((pattern) => pattern.test(compact));
}

function historyArtifactToolCall(text: string, index: number): ToolCallInfo {
  const isError = /\b(error|failed|failure|timeout)\b|失败|异常|超时/.test(
    text,
  );
  return {
    index,
    name: "history_progress",
    source: "builtin",
    args: JSON.stringify({ source: "history_replay" }),
    result: text.trim(),
    isError,
    status: isError ? "error" : "success",
  };
}

function mapHistoryMessages(
  messages: any[],
  options?: {
    source?: string;
    idPrefix?: string;
  },
): DisplayMessage[] {
  const source = options?.source ?? "chat";
  const idPrefix = options?.idPrefix ?? "hist";
  const merged: DisplayMessage[] = [];
  let pendingToolCalls: ToolCallInfo[] = [];
  let pendingThinking: string | null = null;
  let _toolIdCounter = 0;
  const fallbackTimestampBase = Date.now() - messages.length * 1000;
  let fallbackTimestampIndex = 0;
  const nextFallbackTimestamp = () =>
    fallbackTimestampBase + fallbackTimestampIndex++ * 1000;

  const attachArtifactsToLastAssistant = (
    toolCalls?: ToolCallInfo[],
    thinking?: string | null,
  ): boolean => {
    if ((!toolCalls || toolCalls.length === 0) && !thinking) return false;
    for (let i = merged.length - 1; i >= 0; i -= 1) {
      if (merged[i].role !== "assistant") continue;
      merged[i] = withAssistantArtifacts(merged[i], toolCalls, thinking);
      return true;
    }
    return false;
  };

  const flushPendingAssistantArtifacts = (
    mode: "previous" | "synthetic" = "previous",
  ) => {
    if (pendingToolCalls.length === 0 && !pendingThinking) return;
    if (attachArtifactsToLastAssistant(pendingToolCalls, pendingThinking)) {
      pendingToolCalls = [];
      pendingThinking = null;
      return;
    }
    if (mode === "synthetic" && pendingToolCalls.length > 0) {
      merged.push({
        id: `${idPrefix}-assistant-${merged.length}`,
        role: "assistant",
        content: "",
        timestamp: nextFallbackTimestamp(),
        toolCalls: pendingToolCalls,
        thinking: pendingThinking,
      });
      pendingToolCalls = [];
      pendingThinking = null;
    }
  };

  const queueOrAttachToolCalls = (toolCalls: ToolCallInfo[]) => {
    if (toolCalls.length === 0) return;
    const last = merged[merged.length - 1];
    if (last?.role === "assistant") {
      merged[merged.length - 1] = withAssistantArtifacts(last, toolCalls);
      return;
    }
    pendingToolCalls = mergeHistoryToolCalls(pendingToolCalls, toolCalls);
  };

  const queueOrAttachThinking = (thinking: string) => {
    const normalized = thinking.trim();
    if (!normalized) return;
    const last = merged[merged.length - 1];
    if (last?.role === "assistant") {
      merged[merged.length - 1] = withAssistantArtifacts(
        last,
        undefined,
        normalized,
      );
      return;
    }
    pendingThinking = mergeHistoryThinking(pendingThinking, normalized);
  };

  for (const msg of messages) {
    if (msg.role === "tool") continue;
    if (msg.role !== "user" && msg.role !== "assistant") continue;

    const rawToolCalls: ToolCallInfo[] = (msg.tool_calls ?? []).map(
      (tc: any) => {
        const result = tc.result ?? { output: "", is_error: false };
        const parsed = parseToolName(tc.name);
        return {
          index: _toolIdCounter++,
          name: parsed.tool,
          source: parsed.source,
          mcpServer: parsed.source === "mcp" ? parsed.sourceName : undefined,
          skillName: parsed.source === "skill" ? parsed.sourceName : undefined,
          args: tc.input,
          result: result.output,
          isError: result.is_error,
          status: "success" as const,
        };
      },
    );

    const rawContent = parsePersistedHistoryContent(msg.content);
    const plainContent = historyContentToPlain(rawContent);
    const hasContent = plainContent.trim().length > 0;
    if (
      source === "pm" &&
      msg.role === "assistant" &&
      hasContent &&
      isInternalPmContractText(plainContent)
    ) {
      continue;
    }

    if (msg.role === "assistant" && hasContent) {
      const thinkingText = extractHistoricalThinkingText(plainContent);
      if (thinkingText) {
        if (rawToolCalls.length > 0) queueOrAttachToolCalls(rawToolCalls);
        queueOrAttachThinking(thinkingText);
        continue;
      }

      if (isHistoricalToolBridgeText(plainContent)) {
        queueOrAttachToolCalls(
          rawToolCalls.length > 0
            ? rawToolCalls
            : [historyArtifactToolCall(plainContent, _toolIdCounter++)],
        );
        continue;
      }
    }

    if (hasContent) {
      if (msg.role === "user") {
        merged.push({
          id: `${idPrefix}-${msg.role}-${merged.length}`,
          role: "user",
          content: rawContent,
          timestamp: pickMessageTimestampMs(msg, nextFallbackTimestamp()),
          createdAt: msg.createdAt ?? msg.created_at ?? null,
          pmReport: normalizePmReportArtifact(
            (msg as Record<string, unknown>).pm_report,
          ),
        });
        continue;
      }

      const allToolCalls = mergeHistoryToolCalls(
        pendingToolCalls,
        rawToolCalls,
      );
      const allThinking = mergeHistoryThinking(
        pendingThinking,
        typeof msg.thinking === "string" ? msg.thinking : null,
      );
      pendingToolCalls = [];
      pendingThinking = null;
      merged.push({
        id: `${idPrefix}-${msg.role}-${merged.length}`,
        role: "assistant",
        content: msg.content,
        timestamp: pickMessageTimestampMs(msg, nextFallbackTimestamp()),
        createdAt: msg.createdAt ?? msg.created_at ?? null,
        toolCalls: allToolCalls.length > 0 ? allToolCalls : undefined,
        thinking: allThinking,
        pmTaskId:
          typeof msg.pm_task_id === "string" && msg.pm_task_id.trim().length > 0
            ? msg.pm_task_id.trim()
            : undefined,
        pmTaskStatus:
          typeof msg.pm_task_status === "string" &&
          msg.pm_task_status.trim().length > 0
            ? msg.pm_task_status.trim()
            : undefined,
        pmReport: normalizePmReportArtifact(
          (msg as Record<string, unknown>).pm_report,
        ),
      });
    } else {
      if (rawToolCalls.length > 0) {
        pendingToolCalls = mergeHistoryToolCalls(
          pendingToolCalls,
          rawToolCalls,
        );
      }
      if (typeof msg.thinking === "string" && msg.thinking.trim()) {
        pendingThinking = mergeHistoryThinking(pendingThinking, msg.thinking);
      }
    }
  }
  flushPendingAssistantArtifacts("synthetic");

  const normalized: DisplayMessage[] = [];
  for (const msg of merged) {
    const last = normalized[normalized.length - 1];
    if (!last) {
      normalized.push(msg);
      continue;
    }

    const currPlain = historyContentToPlain(msg.content).trim();
    const lastPlain = historyContentToPlain(last.content).trim();

    if (msg.role === "assistant" && last.role === "assistant") {
      const lastIsProgress = isHistoryProgressOnlyAssistant(last);
      const currIsProgress = isHistoryProgressOnlyAssistant(msg);
      if (lastIsProgress && !currIsProgress) {
        const progressToolCalls =
          last.toolCalls && last.toolCalls.length > 0
            ? last.toolCalls
            : lastPlain
              ? [historyArtifactToolCall(lastPlain, _toolIdCounter++)]
              : undefined;
        normalized[normalized.length - 1] = mergeHistoryAssistantMessages(
          { ...last, toolCalls: progressToolCalls },
          msg,
        );
        continue;
      }
      if (!lastIsProgress && currIsProgress) {
        const progressToolCalls =
          msg.toolCalls && msg.toolCalls.length > 0
            ? msg.toolCalls
            : currPlain
              ? [historyArtifactToolCall(currPlain, _toolIdCounter++)]
              : undefined;
        normalized[normalized.length - 1] = mergeHistoryAssistantMessages(msg, {
          ...last,
          toolCalls: mergeHistoryToolCalls(last.toolCalls, progressToolCalls),
          thinking: mergeHistoryThinking(last.thinking, msg.thinking),
        });
        continue;
      }
    }

    if (msg.role === last.role && currPlain === lastPlain) {
      const toolCalls = mergeHistoryToolCalls(last.toolCalls, msg.toolCalls);
      last.toolCalls = toolCalls.length > 0 ? toolCalls : undefined;
      last.thinking = mergeHistoryThinking(last.thinking, msg.thinking);
      if (!last.pmTaskId && msg.pmTaskId) {
        last.pmTaskId = msg.pmTaskId;
      }
      if (!last.pmTaskStatus && msg.pmTaskStatus) {
        last.pmTaskStatus = msg.pmTaskStatus;
      }
      if (!last.pmReport && msg.pmReport) {
        last.pmReport = msg.pmReport;
      }
      if (!last.pmFinalDelivery && msg.pmFinalDelivery) {
        last.pmFinalDelivery = msg.pmFinalDelivery;
      }
      if (last.timestamp == null && msg.timestamp != null) {
        last.timestamp = msg.timestamp;
      }
      if (last.createdAt == null && msg.createdAt != null) {
        last.createdAt = msg.createdAt;
      }
      last.pmSearchUsage = mergeDisplayMessageSearchUsage(
        last.pmSearchUsage,
        msg.pmSearchUsage,
      );
      last.traceEvents = [
        ...(last.traceEvents ?? []),
        ...(msg.traceEvents ?? []),
      ].slice(-40);
      continue;
    }

    if (
      source === "pm" &&
      msg.role === "assistant" &&
      last.role === "assistant" &&
      isInternalPmContractText(lastPlain) &&
      !isInternalPmContractText(currPlain)
    ) {
      normalized[normalized.length - 1] = {
        ...msg,
        toolCalls: mergeHistoryToolCalls(last.toolCalls, msg.toolCalls),
        thinking: mergeHistoryThinking(last.thinking, msg.thinking),
        pmTaskId: msg.pmTaskId ?? last.pmTaskId,
        pmTaskStatus: msg.pmTaskStatus ?? last.pmTaskStatus,
        pmReport: msg.pmReport ?? last.pmReport,
        pmFinalDelivery: msg.pmFinalDelivery ?? last.pmFinalDelivery,
        pmSearchUsage: mergeDisplayMessageSearchUsage(
          last.pmSearchUsage,
          msg.pmSearchUsage,
        ),
        traceEvents: [
          ...(last.traceEvents ?? []),
          ...(msg.traceEvents ?? []),
        ].slice(-40),
      };
      continue;
    }

    normalized.push(msg);
  }

  return normalized;
}

export function attachPmFinalDeliveryArtifacts(
  messages: DisplayMessage[],
  rawArtifacts: unknown,
): DisplayMessage[] {
  if (!Array.isArray(rawArtifacts) || rawArtifacts.length === 0) return messages;
  const byTaskId = new Map<string, PmFinalDeliveryArtifact>();
  for (const raw of rawArtifacts) {
    if (!raw || typeof raw !== "object") continue;
    const value = raw as Partial<PmFinalDeliveryArtifact>;
    if (typeof value.taskId !== "string" || !value.taskId.trim()) continue;
    if (typeof value.contentHash !== "string") continue;
    byTaskId.set(value.taskId, value as PmFinalDeliveryArtifact);
  }
  if (byTaskId.size === 0) return messages;
  const attached = new Set<string>();
  const next = messages.map((message) => {
    if (message.role !== "assistant") return message;
    const taskId = message.pmTaskId;
    let artifact = taskId ? byTaskId.get(taskId) : undefined;
    // Older history rows may not carry pm_task_id even though the backend
    // reconstructed a durable task binding. Match the persisted terminal text
    // before creating a synthetic row, so refresh never loses the report card.
    if (!artifact) {
      const messageText = historyContentToPlain(message.content).trim();
      if (messageText) {
        artifact = [...byTaskId.values()].find(
          (candidate) =>
            !attached.has(candidate.taskId) &&
            candidate.response?.text?.trim() === messageText,
        );
      }
    }
    if (!artifact) return message;
    attached.add(artifact.taskId);
    const restoredReport = normalizePmReportArtifact(
      artifact.response?.pm_report,
    );
    return {
      ...message,
      pmTaskId: message.pmTaskId ?? artifact.taskId,
      pmTaskStatus: message.pmTaskStatus ?? artifact.taskStatus,
      pmReport: message.pmReport ?? restoredReport,
      pmFinalDelivery: artifact,
    };
  });

  // A legacy session can contain a task row without a visible assistant row
  // (for example after a crash between task persistence and chat projection).
  // Materialize the durable response so the user can still read the complete
  // delivery and its report after reload.
  for (const artifact of byTaskId.values()) {
    if (attached.has(artifact.taskId)) continue;
    const text = artifact.response?.text?.trim();
    if (!text) continue;
    next.push({
      id: `pm-delivery-${artifact.taskId}`,
      role: "assistant",
      content: text,
      pmTaskId: artifact.taskId,
      pmTaskStatus: artifact.taskStatus,
      pmReport: normalizePmReportArtifact(artifact.response?.pm_report),
      pmFinalDelivery: artifact,
    });
    attached.add(artifact.taskId);
  }
  return next;
}

function attachSuperAssistantTurnMetadata(
  messages: DisplayMessage[],
  metadata: SuperAssistantTurnMessageMetadata[] | null | undefined,
): DisplayMessage[] {
  if (!metadata?.length) return messages;
  const byAnswer = new Map<string, SuperAssistantTurnMessageMetadata[]>();
  for (const item of metadata) {
    const key = item.final_text.trim();
    if (!key) continue;
    const bucket = byAnswer.get(key) ?? [];
    bucket.push(item);
    byAnswer.set(key, bucket);
  }
  return messages.map((message) => {
    if (message.role !== "assistant") return message;
    const key = historyContentToPlain(message.content).trim();
    const bucket = byAnswer.get(key);
    const item = bucket?.shift();
    if (!item) return message;
    const persistedNl2sqlCalls = nl2sqlAuditToolCallsFromHistory(
      item.nl2sql_audits,
    );
    const toolCalls =
      persistedNl2sqlCalls.length > 0
        ? [
            ...(message.toolCalls ?? []).filter(
              (tool) => !isNl2sqlAuditTool(tool),
            ),
            ...persistedNl2sqlCalls,
          ]
        : message.toolCalls;
    return {
      ...message,
      toolCalls,
      modelName: item.model?.trim() || message.modelName,
      judgeModel: item.judge_model?.trim() || undefined,
      winnerModel: item.winner_model?.trim() || undefined,
      winnerReason: item.winner_reason?.trim() || undefined,
      adversarialRunId: item.adversarial_run_id?.trim() || undefined,
      attributionTaskId: item.attribution_task_id?.trim() || undefined,
      superAssistantTurnId: item.turn_id?.trim() || undefined,
    };
  });
}

function mergeHistoryPages(
  older: DisplayMessage[],
  newer: DisplayMessage[],
): DisplayMessage[] {
  if (older.length === 0) return newer;
  if (newer.length === 0) return older;
  const out = [...older];
  const pushIfDistinct = (msg: DisplayMessage) => {
    const last = out[out.length - 1];
    if (!last) {
      out.push(msg);
      return;
    }
    const lastPlain = historyContentToPlain(last.content).trim();
    const currPlain = historyContentToPlain(msg.content).trim();
    if (last.role === msg.role && lastPlain === currPlain) {
      const toolCalls = mergeHistoryToolCalls(last.toolCalls, msg.toolCalls);
      last.toolCalls = toolCalls.length > 0 ? toolCalls : undefined;
      last.thinking = mergeHistoryThinking(last.thinking, msg.thinking);
      if (!last.pmTaskId && msg.pmTaskId) last.pmTaskId = msg.pmTaskId;
      if (!last.pmTaskStatus && msg.pmTaskStatus) {
        last.pmTaskStatus = msg.pmTaskStatus;
      }
      if (!last.pmReport && msg.pmReport) last.pmReport = msg.pmReport;
      if (!last.pmFinalDelivery && msg.pmFinalDelivery) {
        last.pmFinalDelivery = msg.pmFinalDelivery;
      }
      if (last.timestamp == null && msg.timestamp != null) {
        last.timestamp = msg.timestamp;
      }
      if (last.createdAt == null && msg.createdAt != null) {
        last.createdAt = msg.createdAt;
      }
      return;
    }
    out.push(msg);
  };
  for (const msg of newer) pushIfDistinct(msg);
  return out;
}

// ── Attachment chip ────────────────────────────────────────────────────────────────────────────────

function AttachmentChip({
  block,
  onRemove,
  fileRecord,
}: {
  block: ContentBlock;
  index: number;
  onRemove: () => void;
  fileRecord?: ChatFileRecord;
}) {
  const isImg =
    block.type === "image" && (block as ImageBlock).sourceType === "url";
  const src = isImg
    ? ((block as ImageBlock).previewUrl ?? (block as ImageBlock).data)
    : null;
  const resolvedSrc = useAuthenticatedUploadUrl(src ?? undefined);
  const status = fileRecord?.status;
  const statusColor =
    status === "indexed"
      ? "var(--success, #52c41a)"
      : status === "failed"
        ? "var(--error, #ff4d4f)"
        : status === "parsing"
          ? "var(--warning, #faad14)"
          : "var(--text-muted)";

  return (
    <div
      style={{
        position: "relative",
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        padding: "4px 8px 4px 6px",
        background: "var(--bg-elevated)",
        borderRadius: 8,
        fontSize: 12,
        border: "1px solid var(--border-default)",
      }}
    >
      {resolvedSrc ? (
        <img
          src={resolvedSrc}
          alt="attachment"
          style={{
            width: 24,
            height: 24,
            objectFit: "cover",
            borderRadius: 4,
          }}
        />
      ) : (
        <span style={{ color: "var(--text-secondary)", fontSize: 14 }}>📎</span>
      )}
      <span
        style={{
          maxWidth: 96,
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {block.type === "document"
          ? ((block as DocumentBlock).name ?? "Document")
          : block.type === "image"
            ? ((block as ImageBlock).name ?? "Image")
            : "File"}
      </span>
      {block.type === "document" && status && (
        <Tooltip title={fileRecord?.errorMessage || status}>
          <span
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 4,
              color: statusColor,
              fontSize: 11,
              maxWidth: 72,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {status === "parsing" && (
              <LoadingOutlined style={{ fontSize: 10 }} />
            )}
            {status}
          </span>
        </Tooltip>
      )}
      <button
        onClick={onRemove}
        style={{
          background: "none",
          border: "none",
          cursor: "pointer",
          color: "var(--text-muted)",
          fontSize: 14,
          lineHeight: 1,
          padding: 0,
          display: "flex",
          alignItems: "center",
        }}
      >
        ✕
      </button>
    </div>
  );
}

// ── Main component ────────────────────────────────────────────────────────────────────────────────

const { Text } = Typography;

export function ChatCore({
  sessionSource,
  emptySessionText,
  noSessionPlaceholder,
  topBarExtra,
  topBarActions,
  rightPanel,
  rightPanelOpen = false,
  onScrollToBottom,
  onStreamingChange,
  onBeforeStream,
  onStreamFinished,
  onUsage,
  onSessionCreated,
  onActiveSessionChange,
  onAbortRef,
  sidebarWidth = 240,
  showConfigTags = true,
  showMemoryButton = true,
  messageListProps,
  inputAreaProps,
  inputPlaceholder,
  inputHintBar,
  inputToolbarExtra,
  selectedModel,
  superAssistantEndpoint = false,
}: ChatCoreProps) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const pmAssistantModelName = superAssistantEndpoint
    ? t("superAssistant.name", "超级助手")
    : t("operations.assistant", "产运助手");

  // ── Session state ──────────────────────────────────────────────────────────────────────────────
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  // Notify the host whenever the active session changes (created/selected/cleared)
  // so shell-level panels (e.g. Context_Status) can track the current session.
  useEffect(() => {
    onActiveSessionChange?.(activeSessionId);
  }, [activeSessionId, onActiveSessionChange]);
  const [displayMessages, setDisplayMessages] = useState<DisplayMessage[]>([]);
  const [approvalPaused, setApprovalPaused] = useState<RuntimeApprovalPaused | null>(null);
  const [approvalResolvingId, setApprovalResolvingId] = useState<string | null>(null);
  const approvalPausedRef = useRef<RuntimeApprovalPaused | null>(null);
  const streamHandlersRef = useRef<SessionStreamHandlers | null>(null);
  const loadSessionMessagesRef = useRef<((sessionId: string | null) => Promise<void>) | null>(null);
  const activeSessionIdRef = useRef<string | null>(null);
  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);
  const [historyHasMore, setHistoryHasMore] = useState(false);
  const [historyBeforeTurnCursor, setHistoryBeforeTurnCursor] = useState<
    number | null
  >(null);
  const [historyLoadingMore, setHistoryLoadingMore] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const composerRef = useRef<IsolatedComposerTextareaHandle>(null);
  const draftInputRef = useRef("");
  const hasInputRef = useRef(false);
  const [hasInput, setHasInput] = useState(false);
  const syncInputValue = useCallback((value: string) => {
    draftInputRef.current = value;
    const nextHasInput = value.trim().length > 0;
    if (nextHasInput !== hasInputRef.current) {
      hasInputRef.current = nextHasInput;
      setHasInput(nextHasInput);
    }
  }, []);
  const setInput = useCallback(
    (value: string) => {
      syncInputValue(value);
      composerRef.current?.setValue(value);
    },
    [syncInputValue],
  );
  const [streamingText, setStreamingText] = useState("");
  const [visibleStreamingText, setVisibleStreamingText] = useState("");
  const [streamingMessageTimestamp, setStreamingMessageTimestamp] = useState<
    number | null
  >(null);
  const [activeResponseModelName, setActiveResponseModelName] = useState<
    string | null
  >(selectedModel?.trim() || null);
  const [commandModelOverride, setCommandModelOverride] = useState<
    string | null
  >(null);
  const requestedModel = commandModelOverride || selectedModel;
  const activeResponseModelNameRef = useRef<string | null>(
    selectedModel?.trim() || null,
  );
  const rememberResponseModel = useCallback((model: unknown) => {
    if (typeof model !== "string" || !model.trim()) return;
    const normalized = model.trim();
    activeResponseModelNameRef.current = normalized;
    setActiveResponseModelName(normalized);
  }, []);
  const [activeAdversarialMeta, setActiveAdversarialMeta] = useState<{
    judgeModel?: string;
    winnerModel?: string;
    winnerReason?: string;
    adversarialRunId?: string;
  }>({});
  const activeAdversarialMetaRef = useRef(activeAdversarialMeta);
  const rememberAdversarialMeta = useCallback((detail: unknown) => {
    if (!detail || typeof detail !== "object" || Array.isArray(detail)) return;
    const record = detail as Record<string, unknown>;
    const result =
      record.result &&
      typeof record.result === "object" &&
      !Array.isArray(record.result)
        ? (record.result as Record<string, unknown>)
        : record;
    const judgeModel =
      typeof result.judgeModel === "string" ? result.judgeModel.trim() : "";
    const winnerModel =
      typeof result.winnerModel === "string" ? result.winnerModel.trim() : "";
    const winnerReason =
      typeof result.winnerReason === "string" ? result.winnerReason.trim() : "";
    const adversarialRunId =
      typeof record.externalTaskId === "string"
        ? record.externalTaskId.trim()
        : typeof result.runId === "string"
          ? result.runId.trim()
          : "";
    if (!judgeModel && !winnerModel && !winnerReason && !adversarialRunId)
      return;
    const next = {
      judgeModel: judgeModel || activeAdversarialMetaRef.current.judgeModel,
      winnerModel: winnerModel || activeAdversarialMetaRef.current.winnerModel,
      winnerReason:
        winnerReason || activeAdversarialMetaRef.current.winnerReason,
      adversarialRunId:
        adversarialRunId || activeAdversarialMetaRef.current.adversarialRunId,
    };
    activeAdversarialMetaRef.current = next;
    setActiveAdversarialMeta(next);
  }, []);
  const [isStreaming, setIsStreaming] = useState(false);
  const [searchMode, setSearchMode] = useState<"on" | "off">("off");
  const [memoryDrawerOpen, setMemoryDrawerOpen] = useState(false);
  const [memorySourceGroup, setMemorySourceGroup] = useState<
    "manual" | "automatic"
  >("manual");
  const [memoryDraft, setMemoryDraft] = useState("");
  const [memoryCreating, setMemoryCreating] = useState(false);
  const [memoryModeUpdating, setMemoryModeUpdating] = useState(false);
  const [memoryDeletingId, setMemoryDeletingId] = useState<string | null>(null);
  const [contextCompacting, setContextCompacting] = useState(false);
  const [lastManualCompaction, setLastManualCompaction] =
    useState<AgentManualCompactionResult | null>(null);
  const [chatFileRecords, setChatFileRecords] = useState<
    Record<string, ChatFileRecord>
  >({});
  const [pmStageStates, setPmStageStates] = useState<
    Record<string, PmStageState>
  >({});
  const [pmStageEvents, setPmStageEvents] = useState<PmStageEvent[]>([]);
  const [pmSubtaskRows, setPmSubtaskRows] = useState<ApiPmSubtaskRuntimeRow[]>(
    [],
  );
  const [pmSubtaskAttempts, setPmSubtaskAttempts] = useState<
    Record<string, ApiPmSubtaskAttemptRow[]>
  >({});
  const [pmQualitySnapshot, setPmQualitySnapshot] =
    useState<PmQualitySnapshot | null>(null);
  const [pmBackgroundTaskId, setPmBackgroundTaskId] = useState<string | null>(
    null,
  );
  const [pmBackgroundTaskStatus, setPmBackgroundTaskStatus] = useState<
    string | null
  >(null);
  const [pmPromptQueue, setPmPromptQueue] = useState<PmQueuedPrompt[]>([]);
  const [pmPanelTaskId, setPmPanelTaskId] = useState<string | null>(null);
  const [pmPanelTaskStatus, setPmPanelTaskStatus] = useState<string | null>(
    null,
  );
  const [pmPanelOpen, setPmPanelOpen] = useState(false);
  const [pmSuppressExecutionUi, setPmSuppressExecutionUi] = useState(false);
  const [pmInlineSegments, setPmInlineSegments] = useState<PmInlineSegment[]>(
    [],
  );
  const [pmSelectedStageId, setPmSelectedStageId] = useState<string | null>(
    null,
  );
  const [pmShowAllExecutionDetails, setPmShowAllExecutionDetails] =
    useState(false);
  const [pmSelectedClaimIndex, setPmSelectedClaimIndex] = useState<
    number | null
  >(null);
  const [pmStrategyLeaderboardRows, setPmStrategyLeaderboardRows] = useState<
    PmStrategyLeaderboardRow[]
  >([]);
  const [toolCalls, setToolCalls] = useState<Record<string, ToolCallInfo>>({});
  const [attachments, setAttachments] = useState<ContentBlock[]>([]);
  const [uploading, setUploading] = useState(false);
  const [slashOpen, setSlashOpen] = useState(false);
  const [slashFilter, setSlashFilter] = useState("");
  const [slashSelected, setSlashSelected] = useState(0);
  const [thinkingText, setThinkingText] = useState("");
  const [thinkingExpanded, setThinkingExpanded] = useState(false);
  const [thinkingLoading, setThinkingLoading] = useState(false);
  /**
   * Wall-clock duration of the current reasoning stream. Driven by
   * `thinkingStartedAtRef` — set at `thinking_start`, frozen at either
   * `thinking_end` or the first `text_delta` that closed the block.
   * Rendered in the bubble's "已深度思考 · Xs" pill.
   */
  const [thinkingDurationMs, setThinkingDurationMs] = useState<
    number | undefined
  >(undefined);
  const [activeMcpServers, setActiveMcpServers] = useState<string[]>([]);
  const [activeSkills, setActiveSkills] = useState<string[]>([]);
  const [draggingOver, setDraggingOver] = useState(false);
  const [replyingTo, setReplyingTo] = useState<string | null>(null);
  const [pmRuntimeTick, setPmRuntimeTick] = useState<number>(Date.now());

  // ── Refs ────────────────────────────────────────────────────────────────────────────────────────
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messageListRef = useRef<HTMLDivElement>(null);
  const abortRef = useRef<(() => void) | null>(null);
  useEffect(() => {
    approvalPausedRef.current = null;
    setApprovalPaused(null);
    setApprovalResolvingId(null);
    if (streamHandlersRef.current?.sessionId !== activeSessionId) {
      streamHandlersRef.current = null;
    }
    if (!activeSessionId) {
      return;
    }
    let cancelled = false;
    void agentApi
      .listSessionApprovals(activeSessionId)
      .then(({ approvals }) => {
        if (cancelled || approvals.length === 0) return;
        const first = approvals[0];
        const paused: RuntimeApprovalPaused = {
          sessionId: activeSessionId,
          runtimeTurnId: first.turnId,
          approvals,
        };
        approvalPausedRef.current = paused;
        setApprovalPaused(paused);
        setIsStreaming(false);
        onStreamingChange?.(false);
      })
      .catch(() => {
        // A missing approval projection must not prevent the normal history view.
      });
    return () => {
      cancelled = true;
    };
  }, [activeSessionId, onStreamingChange]);

  const resolvePendingApproval = useCallback(
    (requestId: string, decision: "approve" | "deny" | "cancel") => {
      if (!activeSessionId || approvalResolvingId) return;
      // After a page reload there is no previous stream handler to reuse. The
      // durable stream still owns the resume; reload history after its terminal
      // event so the resumed answer and tool outcome are rendered canonically.
      const handlers = approvalResumeHandlers(
        activeSessionId,
        streamHandlersRef.current,
        {
          onApprovalRequired: (paused: RuntimeApprovalPaused) => {
            if (activeSessionIdRef.current !== activeSessionId) return;
            approvalPausedRef.current = paused;
            setApprovalPaused(paused);
            setApprovalResolvingId(null);
            setIsStreaming(false);
            onStreamingChange?.(false);
          },
          onStreamEnd: () => {
            if (activeSessionIdRef.current !== activeSessionId) return;
            approvalPausedRef.current = null;
            setApprovalPaused(null);
            if (streamHandlersRef.current?.sessionId === activeSessionId) {
              streamHandlersRef.current = null;
            }
            setApprovalResolvingId(null);
            setIsStreaming(false);
            onStreamingChange?.(false);
            void loadSessionMessagesRef.current?.(activeSessionId);
          },
          onError: () => {
            if (activeSessionIdRef.current !== activeSessionId) return;
            if (streamHandlersRef.current?.sessionId === activeSessionId) {
              streamHandlersRef.current = null;
            }
            setApprovalResolvingId(null);
            setIsStreaming(false);
            onStreamingChange?.(false);
          },
        },
      );
      setApprovalResolvingId(requestId);
      setIsStreaming(true);
      onStreamingChange?.(true);
      const abort = streamAgentSession(activeSessionId, "", handlers, {
        approval: { requestId, decision },
      });
      abortRef.current = abort;
    },
    [activeSessionId, approvalResolvingId, onStreamingChange],
  );
  const superAssistantTurnIdRef = useRef<string | null>(null);
  const pmBackgroundTaskIdRef = useRef<string | null>(null);
  const stopTurnInFlightRef = useRef(false);
  const toolCallsRef = useRef<Record<string, ToolCallInfo>>({});
  const liveToolIndicesRef = useRef<Set<string>>(new Set());
  const liveToolKeyByIndexRef = useRef<Record<number, string>>({});
  const fileInputRef = useRef<HTMLInputElement>(null);
  const isComposingRef = useRef(false);
  const thinkingTextRef = useRef("");
  const thinkingLoadingRef = useRef(false);
  const syntheticThinkingHintRef = useRef(false);
  /**
   * Timestamp (ms) of the last `thinking_start`. Reset to `null` when the
   * reasoning stream closes. Used to compute the final duration without
   * relying on re-renders.
   */
  const thinkingStartedAtRef = useRef<number | null>(null);
  const pmStageStatesRef = useRef<Record<string, PmStageState>>({});
  const pmStageEventsRef = useRef<PmStageEvent[]>([]);
  const pmQualitySnapshotRef = useRef<PmQualitySnapshot | null>(null);
  const pmBackgroundTaskAbortRef = useRef<(() => void) | null>(null);
  const pmPromptQueueRef = useRef<PmQueuedPrompt[]>([]);
  const pmQueueStartingRef = useRef(false);
  const pmQueuedUserMessageIdsRef = useRef<Set<string>>(new Set());
  const pmUserMessageIdByTaskIdRef = useRef<Record<string, string>>({});
  const pmPanelReplayAbortRef = useRef<(() => void) | null>(null);
  const pmInlineSegmentsRef = useRef<PmInlineSegment[]>([]);
  const pmActiveInlineSegmentIdRef = useRef<string | null>(null);
  const pmInlineActionByToolKeyRef = useRef<Record<string, string>>({});
  const pmRecordedRunKeysRef = useRef<Set<string>>(new Set());
  const pmImageContextWarningKeysRef = useRef<Set<string>>(new Set());
  const pmPipelineStartedAtRef = useRef<number | null>(null);
  const pmSubtaskLastRefreshAtRef = useRef<number>(0);
  const streamingTextRef = useRef("");
  const visibleStreamingTextRef = useRef("");
  const typewriterTimerRef = useRef<number | null>(null);
  const typewriterOnDrainedRef = useRef<(() => void) | null>(null);
  const latestRequestRef = useRef<string | null>(null);
  const streamCommittedRef = useRef(false);
  const lastStreamActivityAtRef = useRef<number>(Date.now());
  const streamRecoveryInFlightRef = useRef(false);
  const superAssistantAsyncTaskStartedRef = useRef(false);
  /**
   * Frozen duration once the thinking stream closed. Kept in a ref so we
   * can hand it to the persisted assistant message at `onStreamEnd` even
   * though the React state may already have been reset by that point.
   */
  const thinkingDurationRef = useRef<number | undefined>(undefined);
  const autoFollowScrollRef = useRef(true);
  const messageTextSelectionActiveRef = useRef(false);
  const historyAutoFillRef = useRef(false);

  const refreshSessionMemorySources = useCallback(
    async (sessionId: string, sinceMs?: number) => {
      try {
        const resp = await agentApi.listSessionMemoryCitations(sessionId);
        setDisplayMessages((prev) =>
          attachSessionMemoryCitations(prev, resp.items ?? [], { sinceMs }),
        );
      } catch {
        // Memory citations are best-effort; never disturb the visible answer.
      }
    },
    [],
  );

  const refreshLatestChatArtifacts = useCallback(
    async (sessionId: string, targetMessageId?: string) => {
      try {
        const [evidence, trace] = await Promise.all([
          agentApi.getChatSessionEvidence(sessionId),
          agentApi.getChatSessionTrace(sessionId),
        ]);
        setDisplayMessages((prev) =>
          attachChatArtifactsToLatestAssistant(
            prev,
            latestChatArtifactPayloads(evidence.items),
            latestChatArtifactPayloads(trace.items),
            targetMessageId,
          ),
        );
      } catch {
        // Artifacts are best-effort; the answer itself should remain stable.
      }
    },
    [],
  );
  const historyLoadingMoreRef = useRef(false);

  /**
   * Reset every piece of live reasoning state (refs + React state) to
   * its pristine form. Called from session swaps, new-session clicks,
   * send start, and error handlers — anywhere we transition back into
   * "no reasoning in flight". Centralising the reset prevents the
   * stale-loader bug that users saw when one of the six previous
   * reset sites missed a field.
   */
  const resetThinkingState = useCallback(() => {
    setThinkingText("");
    thinkingTextRef.current = "";
    setThinkingExpanded(false);
    setThinkingLoading(false);
    thinkingLoadingRef.current = false;
    setThinkingDurationMs(undefined);
    thinkingDurationRef.current = undefined;
    thinkingStartedAtRef.current = null;
    syntheticThinkingHintRef.current = false;
  }, []);

  const resetPmResearchState = useCallback(() => {
    if (pmBackgroundTaskAbortRef.current) {
      pmBackgroundTaskAbortRef.current();
      pmBackgroundTaskAbortRef.current = null;
    }
    if (pmPanelReplayAbortRef.current) {
      pmPanelReplayAbortRef.current();
      pmPanelReplayAbortRef.current = null;
    }
    pmBackgroundTaskIdRef.current = null;
    setPmBackgroundTaskId(null);
    setPmBackgroundTaskStatus(null);
    setPmPanelTaskId(null);
    setPmPanelTaskStatus(null);
    setPmStageStates({});
    pmStageStatesRef.current = {};
    setPmStageEvents([]);
    pmStageEventsRef.current = [];
    setPmSubtaskRows([]);
    setPmSubtaskAttempts({});
    pmSubtaskLastRefreshAtRef.current = 0;
    setPmQualitySnapshot(null);
    pmQualitySnapshotRef.current = null;
    setPmPanelOpen(false);
    setPmSuppressExecutionUi(false);
    setPmInlineSegments([]);
    setPmSelectedStageId(null);
    setPmShowAllExecutionDetails(false);
    setPmSelectedClaimIndex(null);
    pmInlineSegmentsRef.current = [];
    pmActiveInlineSegmentIdRef.current = null;
    pmInlineActionByToolKeyRef.current = {};
    liveToolKeyByIndexRef.current = {};
    pmImageContextWarningKeysRef.current.clear();
    pmPipelineStartedAtRef.current = null;
    superAssistantAsyncTaskStartedRef.current = false;
    activeAdversarialMetaRef.current = {};
    setActiveAdversarialMeta({});
    setPmRuntimeTick(Date.now());
  }, []);

  const clearPmPromptQueue = useCallback(() => {
    pmPromptQueueRef.current = [];
    pmQueuedUserMessageIdsRef.current.clear();
    pmUserMessageIdByTaskIdRef.current = {};
    setPmPromptQueue([]);
  }, []);

  const commitPmPromptQueue = useCallback((next: PmQueuedPrompt[]) => {
    pmPromptQueueRef.current = next;
    setPmPromptQueue(next);
  }, []);

  const handlePmImageContextWarning = useCallback(
    (payload?: { message?: string; detail?: string; code?: string }) => {
      const dedupKey = `${pmBackgroundTaskId ?? "pm"}|${payload?.code ?? ""}|${payload?.detail ?? payload?.message ?? ""}`;
      if (pmImageContextWarningKeysRef.current.has(dedupKey)) return;
      pmImageContextWarningKeysRef.current.add(dedupKey);
      message.warning(
        payload?.message ||
          t(
            "chat.imageContextWarningDefault",
            "图片解析部分失败，系统将继续基于可用信息回答。",
          ),
      );
    },
    [pmBackgroundTaskId, t],
  );

  useEffect(() => {
    if (sessionSource !== "pm") return;
    const hasRunningStage = Object.values(pmStageStates).some(
      (stage) => stage?.status === "running",
    );
    if (!hasRunningStage) return;
    const timer = window.setInterval(() => {
      setPmRuntimeTick(Date.now());
    }, 1000);
    return () => window.clearInterval(timer);
  }, [pmStageStates, sessionSource]);

  const refreshPmStrategyLeaderboard = useCallback(async () => {
    if (sessionSource !== "pm") return;
    try {
      const resp = await agentApi.listPmStrategyLeaderboard();
      const rows: PmStrategyLeaderboardRow[] = (resp.rows ?? []).map((row) => {
        const latestAt =
          typeof row.lastRunAt === "string" && row.lastRunAt.trim().length > 0
            ? Date.parse(row.lastRunAt)
            : Date.now();
        return {
          routeKey: buildRouteKey(row.route, row.channel),
          route: row.route ?? "auto_route",
          channel: row.channel,
          runs: Number.isFinite(row.runCount) ? Math.max(0, row.runCount) : 0,
          passRate: Number.isFinite(row.successRate)
            ? Math.max(0, Math.min(1, row.successRate))
            : 0,
          avgCitationCount: Number.isFinite(row.avgQuality)
            ? Math.max(0, row.avgQuality * 8)
            : 0,
          avgDomainCount: Number.isFinite(row.avgQuality)
            ? Math.max(0, row.avgQuality * 4)
            : 0,
          avgRetrieveDurationMs: Number.isFinite(row.avgRetrieveDurationMs)
            ? Math.max(0, Math.round(row.avgRetrieveDurationMs))
            : null,
          score: Number.isFinite(row.score) ? row.score * 100 : 0,
          latestAt: Number.isFinite(latestAt) ? latestAt : Date.now(),
        };
      });
      setPmStrategyLeaderboardRows(rows.slice(0, 10));
    } catch {
      // ignore transient leaderboard fetch errors
    }
  }, [sessionSource]);

  useEffect(() => {
    void refreshPmStrategyLeaderboard();
  }, [refreshPmStrategyLeaderboard]);

  const recordPmStrategyOutcome = useCallback(
    (record: PmStrategyRunRecord) => {
      if (sessionSource !== "pm") return;
      if (pmRecordedRunKeysRef.current.has(record.key)) return;
      pmRecordedRunKeysRef.current.add(record.key);
      void agentApi
        .recordPmStrategyOutcome({
          route: record.route,
          channel: record.channel,
          variant: record.variant,
          passed: record.passed,
          citation_count: record.citationCount,
          domain_count: record.domainCount,
          tool_call_count: record.toolCallCount,
          retrieve_duration_ms: record.retrieveDurationMs,
        })
        .then(() => refreshPmStrategyLeaderboard())
        .catch(() => {
          // ignore strategy recording errors
        });
    },
    [refreshPmStrategyLeaderboard, sessionSource],
  );

  const updatePmInlineSegments = useCallback(
    (updater: (prev: PmInlineSegment[]) => PmInlineSegment[]) => {
      setPmInlineSegments((prev) => {
        const next = updater(prev);
        pmInlineSegmentsRef.current = next;
        return next;
      });
    },
    [],
  );

  const ensurePmInlineSegment = useCallback(
    (
      stage: string,
      status: PmStageStatus,
      attempt: number,
      summary?: string,
      rawDetail?: Record<string, unknown>,
    ): string => {
      const id = `${stage}#${attempt}`;
      const now = Date.now();
      updatePmInlineSegments((prev) => {
        const idx = prev.findIndex((seg) => seg.id === id);
        if (idx >= 0) {
          const next = [...prev];
          next[idx] = {
            ...next[idx],
            status,
            summary:
              summary && summary.trim().length > 0
                ? summary
                : next[idx].summary || fallbackStageNarrative(stage, status),
            rawDetail: rawDetail ?? next[idx].rawDetail,
            updatedAt: now,
          };
          return next;
        }
        return [
          ...prev,
          {
            id,
            stage,
            status,
            attempt,
            summary:
              summary && summary.trim().length > 0
                ? summary
                : fallbackStageNarrative(stage, status),
            rawDetail,
            excerpt: "",
            actions: [],
            createdAt: now,
            updatedAt: now,
          },
        ];
      });
      return id;
    },
    [updatePmInlineSegments],
  );

  const appendPmInlineExcerpt = useCallback(
    (delta: string) => {
      if (!delta || sessionSource !== "pm") return;
      const targetId =
        pmActiveInlineSegmentIdRef.current ??
        pmInlineSegmentsRef.current[pmInlineSegmentsRef.current.length - 1]?.id;
      if (!targetId) return;
      updatePmInlineSegments((prev) =>
        prev.map((seg) =>
          seg.id === targetId
            ? {
                ...seg,
                excerpt: `${seg.excerpt}${delta}`.slice(-4000),
                updatedAt: Date.now(),
              }
            : seg,
        ),
      );
    },
    [sessionSource, updatePmInlineSegments],
  );

  const upsertPmInlineAction = useCallback(
    (
      toolKey: string,
      toolIndex: number,
      patch: Partial<PmInlineAction> & {
        name?: string;
        source?: PmInlineAction["source"];
      },
    ) => {
      if (sessionSource !== "pm") return;
      let segmentId =
        pmActiveInlineSegmentIdRef.current ??
        pmInlineSegmentsRef.current[pmInlineSegmentsRef.current.length - 1]?.id;
      if (!segmentId) {
        segmentId = ensurePmInlineSegment("retrieve", "running", 1);
        pmActiveInlineSegmentIdRef.current = segmentId;
      }
      const existingActionId = pmInlineActionByToolKeyRef.current[toolKey];
      if (existingActionId) {
        const owner = pmInlineSegmentsRef.current.find((segment) =>
          segment.actions.some((action) => action.id === existingActionId),
        );
        if (owner) segmentId = owner.id;
      }
      const actionId =
        existingActionId ?? `${segmentId}:tool:${toolIndex}:${Date.now()}`;
      pmInlineActionByToolKeyRef.current[toolKey] = actionId;
      updatePmInlineSegments((prev) =>
        prev.map((seg) => {
          if (seg.id !== segmentId) {
            const actions = seg.actions.filter(
              (action) => action.id !== actionId,
            );
            return actions.length === seg.actions.length
              ? seg
              : { ...seg, actions };
          }
          const actionIdx = seg.actions.findIndex((a) => a.id === actionId);
          if (actionIdx >= 0) {
            const nextActions = [...seg.actions];
            nextActions[actionIdx] = {
              ...nextActions[actionIdx],
              ...patch,
              updatedAt: Date.now(),
            };
            return { ...seg, actions: nextActions, updatedAt: Date.now() };
          }
          const parsed = patch.name ? parseToolName(patch.name) : null;
          const source =
            patch.source ??
            (parsed?.source as PmInlineAction["source"] | undefined) ??
            "builtin";
          const sourceLabel =
            source === "mcp"
              ? `MCP ${parsed?.sourceName || ""}`.trim()
              : source === "skill"
                ? `Skill ${parsed?.sourceName || ""}`.trim()
                : "builtin";
          const newAction: PmInlineAction = {
            id: actionId,
            index: toolIndex,
            name: parsed?.tool ?? patch.name ?? "tool",
            source,
            sourceLabel,
            status: patch.status ?? "pending",
            durationMs: patch.durationMs,
            detail: patch.detail,
            createdAt: Date.now(),
            updatedAt: Date.now(),
          };
          return {
            ...seg,
            actions: [...seg.actions, newAction],
            updatedAt: Date.now(),
          };
        }),
      );
    },
    [ensurePmInlineSegment, sessionSource, updatePmInlineSegments],
  );

  const describePmInlineAction = useCallback(
    (
      rawToolName: string,
      rawArgs: string,
      rawResult: string,
      status: PmInlineAction["status"],
    ): string => {
      const stage = inferToolStageForPm(rawToolName);
      const target =
        extractPmTargetFromArgs(rawArgs) ?? extractPmTargetFromArgs(rawResult);
      const resultPreview = summarizePmToolResult(rawResult);

      const verb =
        stage === "search"
          ? t("chat.toolVerbSearch", "搜索")
          : stage === "extract"
            ? t("chat.toolVerbExtract", "提取")
            : stage === "verify"
              ? t("chat.toolVerbVerify", "校验")
              : stage === "analyze"
                ? t("chat.toolVerbAnalyze", "分析")
                : stage === "synthesize"
                  ? t("chat.toolVerbSynthesize", "整理")
                  : t("chat.toolVerbExecute", "执行");
      const normalizedTarget = target
        ? `「${target}」`
        : stage === "search" || stage === "extract"
          ? t("chat.pmToolTargetResolving", "正在确认检索目标")
          : shortHumanText(rawToolName.replace(/_/g, " "), 56);

      if (status === "pending") {
        return `${t("chat.toolStatusRunning", "正在")}${verb}: ${normalizedTarget}`;
      }
      if (status === "running") {
        return `${t("chat.toolStatusRunning", "正在")}${verb}: ${normalizedTarget}`;
      }
      if (status === "error") {
        if (resultPreview) {
          return `${verb}${t("chat.toolStatusFailed", "失败")}: ${normalizedTarget} · ${resultPreview}`;
        }
        return `${verb}${t("chat.toolStatusFailed", "失败")}: ${normalizedTarget}`;
      }

      if (resultPreview) {
        return `${t("chat.toolStatusDone", "已完成")}${verb}: ${normalizedTarget} · ${resultPreview}`;
      }
      return `${t("chat.toolStatusDone", "已完成")}${verb}: ${normalizedTarget}`;
    },
    [t],
  );

  const hydratePmInlineFromStageDetail = useCallback(
    (
      segmentId: string,
      stageName: string,
      rawDetail?: Record<string, unknown>,
    ) => {
      if (!rawDetail) return;
      const prefaceText = pickPmDetailString(rawDetail, "prefaceText");
      const previewText = pickPmDetailString(rawDetail, "preview");
      const thinkingText = pickPmDetailString(rawDetail, "thinking");
      const messageText = pickPmDetailString(rawDetail, "message");
      const toolSummary = parsePmToolSummary(rawDetail);
      const liveToolEvent = parsePmLiveToolEvent(rawDetail);
      const now = Date.now();

      updatePmInlineSegments((prev) =>
        prev.map((seg) => {
          if (seg.id !== segmentId) return seg;

          const next: PmInlineSegment = { ...seg, updatedAt: now };
          const excerptBlocks: string[] = [];
          if (stageName === "understand" || stageName === "task_plan") {
            const humanSummaryText = pickPmDetailString(
              rawDetail,
              "humanSummary",
            );
            const planNarrativeBlocks = [
              humanSummaryText,
              prefaceText,
              previewText,
            ]
              .map((block) => (typeof block === "string" ? block.trim() : ""))
              .map(sanitizePmUserFacingStageText)
              .filter((block) => block.length > 0);
            const mergedPlanNarrative =
              mergePmNarrativeBlocks(planNarrativeBlocks);
            if (mergedPlanNarrative) {
              excerptBlocks.push(mergedPlanNarrative);
            }
            if (thinkingText) {
              excerptBlocks.push(
                `${t("chat.thinking", "Thinking")}: ${shortHumanText(thinkingText, 380)}`,
              );
            }
          } else if (previewText) {
            excerptBlocks.push(previewText);
          } else if (messageText) {
            excerptBlocks.push(messageText);
          }
          if (excerptBlocks.length > 0) {
            const merged = excerptBlocks.join("\n").trim();
            if (merged.length > 0) {
              next.excerpt = merged.slice(0, 4000);
            }
          }

          if (liveToolEvent) {
            const sample =
              toolSummary?.samples.find(
                (row) => row.idx === liveToolEvent.index,
              ) ?? toolSummary?.samples[0];
            const source: PmInlineAction["source"] = liveToolEvent.source
              ?.toLowerCase()
              .startsWith("mcp")
              ? "mcp"
              : liveToolEvent.source?.toLowerCase().startsWith("skill")
                ? "skill"
                : "builtin";
            let toolKey = liveToolKeyByIndexRef.current[liveToolEvent.index];
            if (!toolKey) {
              toolKey = `${segmentId}:live:${liveToolEvent.index}`;
              liveToolKeyByIndexRef.current[liveToolEvent.index] = toolKey;
            }
            const actionStatus: PmInlineAction["status"] =
              liveToolEvent.phase === "start"
                ? "running"
                : liveToolEvent.isError
                  ? "error"
                  : "success";
            upsertPmInlineAction(toolKey, liveToolEvent.index, {
              name: liveToolEvent.tool,
              source,
              status: actionStatus,
              durationMs: liveToolEvent.durationMs ?? sample?.durationMs,
              detail: describePmInlineAction(
                liveToolEvent.tool,
                liveToolEvent.target ?? sample?.input ?? "",
                sample?.output ?? "",
                actionStatus === "error"
                  ? "error"
                  : actionStatus === "success"
                    ? "success"
                    : "running",
              ),
            });
          }

          if (!liveToolEvent && toolSummary && toolSummary.samples.length > 0) {
            const preserved = next.actions.filter(
              (action) => !action.id.startsWith(`${segmentId}:sample:`),
            );
            const sampleActions: PmInlineAction[] = toolSummary.samples.map(
              (sample, idx) => {
                const actionId = `${segmentId}:sample:${sample.idx}:${sample.tool}:${idx}`;
                return {
                  id: actionId,
                  index: sample.idx,
                  name: sample.tool,
                  source: sample.source?.startsWith("mcp")
                    ? "mcp"
                    : sample.source?.startsWith("skill")
                      ? "skill"
                      : "builtin",
                  sourceLabel: sample.source ?? "tool",
                  status: sample.isError ? "error" : "success",
                  durationMs: sample.durationMs,
                  detail: describePmInlineAction(
                    sample.tool,
                    sample.input ?? "",
                    sample.output ?? "",
                    sample.isError ? "error" : "success",
                  ),
                  createdAt: now,
                  updatedAt: now,
                };
              },
            );
            next.actions = [...preserved, ...sampleActions];
          }
          return next;
        }),
      );
    },
    [describePmInlineAction, t, updatePmInlineSegments, upsertPmInlineAction],
  );

  const stageLabelForNarrative = useCallback(
    (stageName: string): string => {
      if (stageName.startsWith("data_attribution_")) {
        const attributionStage = stageName.slice("data_attribution_".length);
        if (
          attributionStage === "queued" ||
          attributionStage === "starting" ||
          attributionStage === "wait"
        ) {
          return t("operations.attributionStageQueued", "归因准备");
        }
        if (attributionStage === "understand") {
          return t("operations.attributionStageUnderstand", "归因理解");
        }
        if (attributionStage === "plan") {
          return t("operations.attributionStagePlan", "归因规划");
        }
        if (attributionStage.startsWith("execute")) {
          return t("operations.attributionStageExecute", "归因查数");
        }
        if (attributionStage.startsWith("diagnose")) {
          return t("operations.attributionStageDiagnose", "归因下钻");
        }
        if (attributionStage === "synthesize") {
          return t("operations.attributionStageSynthesize", "归因总结");
        }
        if (attributionStage === "completed") {
          return t("operations.attributionStageCompleted", "归因完成");
        }
        if (attributionStage === "cancelled") {
          return t("operations.attributionStageCancelled", "归因已停止");
        }
        if (attributionStage === "failed") {
          return t("operations.attributionStageFailed", "归因失败");
        }
      }
      if (stageName === "preflight")
        return t("operations.pmStagePreflight", "启动预检");
      if (stageName === "resume")
        return t("operations.pmStageResume", "恢复执行");
      if (stageName === "understand")
        return t("operations.pmStageUnderstand", "任务理解");
      if (stageName === "requirement_state")
        return t("operations.pmRequirementState", "需求状态");
      if (stageName === "report_extract")
        return t("operations.pmStageReportExtract", "报告提取");
      if (stageName === "task_plan")
        return t("operations.pmStageTaskPlan", "任务规划");
      if (stageName === "planner")
        return t("operations.pmStagePlanner", "检索编排");
      if (stageName === "retrieve")
        return t("operations.pmStageRetrieve", "多源检索");
      if (stageName === "deep_loop")
        return t("operations.pmStageDeepLoop", "深度循环");
      if (stageName === "verify")
        return t("operations.pmStageVerify", "证据校验");
      if (stageName === "retry_repair")
        return t("operations.pmStageRetryRepair", "自动修复");
      if (stageName === "turn_model_started") return "思考规划";
      if (stageName === "native_web_search") return "联网检索";
      if (stageName === "runtime_wait") return "执行等待";
      if (stageName === "verification_repair") return "回答校验";
      if (stageName.startsWith("nl2sql_"))
        return t("chat.nl2sqlAuditTitle", "NL2SQL 执行记录");
      if (stageName === "super_adversarial")
        return t("chat.adversarialMode", "超级对抗");
      if (stageName === "synthesize")
        return t("operations.pmStageSynthesize", "总结输出");
      return stageName;
    },
    [t],
  );

  const buildPmStageNarrative = useCallback(
    (
      stageName: string,
      status: PmStageStatus,
      detail?: Record<string, unknown>,
      timingHint?: PmStageTimingHint,
    ): string => {
      const base = toReadableStageDetail(stageName, detail, timingHint, status);
      const label = stageLabelForNarrative(stageName);
      if (base) {
        if (status === "running") {
          return `${label}进行中 · ${base}`;
        }
        if (status === "completed") {
          return `${label}完成 · ${base}`;
        }
        if (status === "failed") {
          return `${label}失败 · ${base}`;
        }
      }
      return fallbackStageNarrative(stageName, status);
    },
    [stageLabelForNarrative],
  );

  const cancelBackendTurn = useCallback(
    async (sessionId?: string | null) => {
      if (!sessionId) return false;
      if (superAssistantEndpoint) {
        let turnId = superAssistantTurnIdRef.current;
        let activeTurn: Awaited<
          ReturnType<typeof agentApi.getSuperAssistantActiveTurn>
        > | null = null;
        if (!turnId) {
          activeTurn = await agentApi.getSuperAssistantActiveTurn(sessionId);
          turnId = activeTurn.active ? (activeTurn.turnId ?? null) : null;
          if (turnId) superAssistantTurnIdRef.current = turnId;
        }
        if (turnId) {
          const result = await agentApi.cancelSuperAssistantTurn(turnId);
          if (superAssistantTurnIdRef.current === turnId) {
            superAssistantTurnIdRef.current = null;
          }
          return result.cancelled;
        }

        // Compatibility fallback for specialist tasks created before the
        // durable parent loop was enabled. New unified turns always cancel via
        // the parent endpoint above so one request owns the whole cascade.
        if (activeTurn?.active && activeTurn.taskId) {
          if (activeTurn.link === "pmResearchTask") {
            await agentApi.cancelPmResearchTask(activeTurn.taskId);
            return true;
          }
          if (activeTurn.link === "chatAdversarialRun") {
            await agentApi.cancelChatAdversarialRun(activeTurn.taskId);
            return true;
          }
          if (activeTurn.link === "dataAttributionTask") {
            await nl2sqlApi.cancelAttributionTask(activeTurn.taskId);
            return true;
          }
        }
      }
      const result = await agentApi.cancelSessionTurn(sessionId);
      // When the durable lookup reports no active parent or specialist, an
      // idle runtime means there is nothing left that can resume after refresh.
      return superAssistantEndpoint ? true : result.cancelled;
    },
    [superAssistantEndpoint],
  );

  const isNearBottom = useCallback((el: HTMLDivElement, threshold = 96) => {
    const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
    return gap <= threshold;
  }, []);

  const hasMessageTextSelection = useCallback(() => {
    const list = messageListRef.current;
    const selection = window.getSelection?.();
    if (!list || !selection || selection.isCollapsed || selection.rangeCount === 0) {
      return false;
    }
    return (
      (!!selection.anchorNode && list.contains(selection.anchorNode)) ||
      (!!selection.focusNode && list.contains(selection.focusNode))
    );
  }, []);

  const shouldPreserveMessageSelection = useCallback(
    () => messageTextSelectionActiveRef.current || hasMessageTextSelection(),
    [hasMessageTextSelection],
  );

  useEffect(() => {
    const syncSelection = () => {
      const active = hasMessageTextSelection();
      messageTextSelectionActiveRef.current = active;
      if (active) autoFollowScrollRef.current = false;
    };
    const finishPointerSelection = () => window.setTimeout(syncSelection, 0);
    document.addEventListener("selectionchange", syncSelection);
    document.addEventListener("pointerup", finishPointerSelection);
    return () => {
      document.removeEventListener("selectionchange", syncSelection);
      document.removeEventListener("pointerup", finishPointerSelection);
    };
  }, [hasMessageTextSelection]);

  const scrollToBottom = useCallback(
    (force = false) => {
      if (shouldPreserveMessageSelection()) {
        autoFollowScrollRef.current = false;
        return;
      }
      const listEl = messageListRef.current;
      if (listEl) {
        if (!force && !autoFollowScrollRef.current) return;
        listEl.scrollTo({
          top: listEl.scrollHeight,
          behavior: force ? "auto" : "smooth",
        });
        onScrollToBottom?.();
        return;
      }
      if (!force && !autoFollowScrollRef.current) return;
      messagesEndRef.current?.scrollIntoView({
        behavior: force ? "auto" : "smooth",
      });
      onScrollToBottom?.();
    },
    [onScrollToBottom, shouldPreserveMessageSelection],
  );

  const focusInputAndScrollToBottom = useCallback(() => {
    autoFollowScrollRef.current = true;
    window.setTimeout(() => {
      if (shouldPreserveMessageSelection()) return;
      scrollToBottom(true);
      composerRef.current?.focus({ preventScroll: true });
    }, 0);
    window.setTimeout(() => {
      if (shouldPreserveMessageSelection()) return;
      scrollToBottom(true);
      composerRef.current?.focus({ preventScroll: true });
    }, 80);
  }, [scrollToBottom, shouldPreserveMessageSelection]);

  const clearTypewriterTimer = useCallback(() => {
    if (typewriterTimerRef.current != null) {
      window.clearTimeout(typewriterTimerRef.current);
      typewriterTimerRef.current = null;
    }
  }, []);

  const resetStreamingText = useCallback(() => {
    clearTypewriterTimer();
    typewriterOnDrainedRef.current = null;
    streamingTextRef.current = "";
    visibleStreamingTextRef.current = "";
    setStreamingText("");
    setVisibleStreamingText("");
    setStreamingMessageTimestamp(null);
  }, [clearTypewriterTimer]);

  const markStreamActivity = useCallback(() => {
    lastStreamActivityAtRef.current = Date.now();
  }, []);

  useEffect(() => {
    pmBackgroundTaskIdRef.current = pmBackgroundTaskId;
  }, [pmBackgroundTaskId]);

  const applySuperAssistantPmStage = useCallback(
    (stage: any) => {
      markStreamActivity();
      rememberResponseModel(stage?.detail?.model);
      if (stage?.detail?.engine === "super_adversarial") {
        rememberAdversarialMeta(stage.detail);
      }
      const stageName = stage?.stage ?? "stage";
      const status = normalizePmStageStatus(stage?.status);
      const attempt =
        typeof stage?.attempt === "number" && stage.attempt > 0
          ? stage.attempt
          : 1;
      const at = Date.now();
      if (pmPipelineStartedAtRef.current == null) {
        pmPipelineStartedAtRef.current = at;
      }
      const previousStageState = pmStageStatesRef.current[stageName];
      const narrativeRunningSince =
        status === "running"
          ? previousStageState?.status === "running" &&
            previousStageState.attempt === attempt
            ? (previousStageState.runningSince ?? previousStageState.updatedAt)
            : at
          : previousStageState?.status === "running" &&
              previousStageState.attempt === attempt
            ? (previousStageState.runningSince ?? previousStageState.updatedAt)
            : previousStageState?.runningSince;
      setPmStageStates((prev) => {
        const previous = prev[stageName];
        const runningSince =
          status === "running"
            ? previous?.status === "running" && previous.attempt === attempt
              ? (previous.runningSince ?? previous.updatedAt)
              : at
            : previous?.status === "running" && previous.attempt === attempt
              ? (previous.runningSince ?? previous.updatedAt)
              : previous?.runningSince;
        const nextEntry: PmStageState = {
          stage: stageName,
          status,
          attempt,
          detail: stage?.detail,
          runningSince,
          updatedAt: at,
        };
        const merged = { ...prev, [stageName]: nextEntry };
        pmStageStatesRef.current = merged;
        return merged;
      });
      setPmStageEvents((prev) => {
        const merged = [
          ...prev,
          {
            stage: stageName,
            status,
            attempt,
            detail: stage?.detail,
            at,
          },
        ].slice(-120);
        pmStageEventsRef.current = merged;
        return merged;
      });
      const summary = buildPmStageNarrative(stageName, status, stage?.detail, {
        nowMs: at,
        runningSinceMs: narrativeRunningSince,
        pipelineStartedAtMs: pmPipelineStartedAtRef.current ?? undefined,
      });
      const rawDetail =
        stage?.detail &&
        typeof stage.detail === "object" &&
        !Array.isArray(stage.detail)
          ? (stage.detail as Record<string, unknown>)
          : undefined;
      const attributionTask = dataAttributionTaskBindingFromStage(stage);
      if (attributionTask) {
        const taskStatus = normalizeAttributionTaskStatus(
          attributionTask.status,
        );
        pmBackgroundTaskIdRef.current = attributionTask.taskId;
        setPmBackgroundTaskId(attributionTask.taskId);
        setPmBackgroundTaskStatus(taskStatus);
        setPmPanelTaskId(attributionTask.taskId);
        setPmPanelTaskStatus(taskStatus);
        setPmSuppressExecutionUi(false);
      }
      const segmentId = ensurePmInlineSegment(
        stageName,
        status,
        attempt,
        summary,
        rawDetail,
      );
      hydratePmInlineFromStageDetail(segmentId, stageName, rawDetail);
      if (isPmLightweightChatDetail(rawDetail)) {
        setPmSuppressExecutionUi(true);
        setPmPanelOpen(false);
      }
      const isRetrieveRunningTransition =
        stageName === "retrieve" &&
        status === "running" &&
        !(
          previousStageState?.status === "running" &&
          previousStageState.attempt === attempt
        );
      if (isRetrieveRunningTransition) {
        liveToolIndicesRef.current = new Set();
        liveToolKeyByIndexRef.current = {};
      }
      if (status === "running") {
        pmActiveInlineSegmentIdRef.current = segmentId;
      } else if (pmActiveInlineSegmentIdRef.current === segmentId) {
        pmActiveInlineSegmentIdRef.current = null;
      }
    },
    [
      buildPmStageNarrative,
      ensurePmInlineSegment,
      hydratePmInlineFromStageDetail,
      markStreamActivity,
      rememberAdversarialMeta,
      rememberResponseModel,
    ],
  );

  const stopActiveTurn = useCallback(
    async (sessionId?: string | null) => {
      if (stopTurnInFlightRef.current) return;
      const targetSessionId = sessionId ?? activeSessionId;
      stopTurnInFlightRef.current = true;
      let cancelled = true;
      try {
        if (superAssistantEndpoint) {
          // Keep the durable event reader open until the server confirms the
          // parent and its subtasks can no longer be recovered as active.
          cancelled = await cancelBackendTurn(targetSessionId);
        } else {
          void cancelBackendTurn(targetSessionId).catch((error) => {
            if (import.meta.env.DEV) {
              console.warn("[ChatCore] cancel session turn failed", error);
            }
          });
        }
      } catch (error) {
        if (import.meta.env.DEV) {
          console.warn("[ChatCore] cancel super assistant turn failed", error);
        }
        message.error(`${t("chat.streamError")}: ${(error as Error).message}`);
        return;
      } finally {
        stopTurnInFlightRef.current = false;
      }
      if (superAssistantEndpoint && !cancelled) return;
      abortRef.current?.();
      abortRef.current = null;
      pmBackgroundTaskAbortRef.current?.();
      pmBackgroundTaskAbortRef.current = null;
      resetStreamingText();
      setToolCalls({});
      toolCallsRef.current = {};
      liveToolIndicesRef.current = new Set();
      liveToolKeyByIndexRef.current = {};
      setIsStreaming(false);
      onStreamingChange?.(false);
      resetThinkingState();
      if (superAssistantEndpoint) {
        const stoppedText = "已停止本次回答。";
        setDisplayMessages((prev) => {
          const last = prev[prev.length - 1];
          if (
            last?.role === "assistant" &&
            contentToPlain(last.content).trim() === stoppedText
          ) {
            return prev;
          }
          return [
            ...prev,
            {
              id: `asst-cancelled-${Date.now()}`,
              role: "assistant",
              content: stoppedText,
              timestamp: Date.now(),
            },
          ];
        });
      }
    },
    [
      activeSessionId,
      cancelBackendTurn,
      onStreamingChange,
      resetStreamingText,
      resetThinkingState,
      superAssistantEndpoint,
      t,
    ],
  );

  // Expose the real stop function to parent controls such as Pipeline Stop.
  useEffect(() => {
    onAbortRef?.(stopActiveTurn);
  }, [onAbortRef, stopActiveTurn]);

  useEffect(() => {
    return () => {
      if (abortRef.current) {
        if (superAssistantEndpoint) {
          // Super Assistant turns are resumable from persisted events. Unmounting
          // or navigating away must only close this browser-side reader; the
          // backend turn keeps running until the user explicitly presses Stop.
          abortRef.current();
          abortRef.current = null;
        } else if (sessionSource !== "pm") {
          stopActiveTurn(activeSessionId);
        }
      }
      if (pmBackgroundTaskAbortRef.current) {
        pmBackgroundTaskAbortRef.current();
        pmBackgroundTaskAbortRef.current = null;
      }
      if (pmPanelReplayAbortRef.current) {
        pmPanelReplayAbortRef.current();
        pmPanelReplayAbortRef.current = null;
      }
    };
  }, [activeSessionId, sessionSource, stopActiveTurn, superAssistantEndpoint]);

  const flushVisibleStreamingText = useCallback(() => {
    clearTypewriterTimer();
    typewriterOnDrainedRef.current = null;
    visibleStreamingTextRef.current = streamingTextRef.current;
    setVisibleStreamingText(streamingTextRef.current);
  }, [clearTypewriterTimer]);

  useEffect(() => {
    const syncVisibleText = () => {
      if (document.visibilityState === "visible") {
        flushVisibleStreamingText();
      }
    };
    document.addEventListener("visibilitychange", syncVisibleText);
    return () => document.removeEventListener("visibilitychange", syncVisibleText);
  }, [flushVisibleStreamingText]);

  const scheduleTypewriterDrain = useCallback(() => {
    if (typewriterTimerRef.current != null) return;

    if (document.hidden) {
      // Do not animate invisible intermediate states. The visibility listener
      // publishes the latest complete snapshot when the tab becomes active.
      visibleStreamingTextRef.current = streamingTextRef.current;
      return;
    }

    const drain = () => {
      typewriterTimerRef.current = null;
      if (shouldPreserveMessageSelection()) {
        typewriterTimerRef.current = window.setTimeout(drain, 80);
        return;
      }
      const raw = streamingTextRef.current;
      const visible = visibleStreamingTextRef.current;

      if (visible.length >= raw.length) {
        const onDrained = typewriterOnDrainedRef.current;
        if (onDrained) {
          typewriterOnDrainedRef.current = null;
          typewriterTimerRef.current = window.setTimeout(() => {
            typewriterTimerRef.current = null;
            onDrained();
          }, 80);
        }
        return;
      }

      const remaining = raw.length - visible.length;
      const charsThisTick = Math.min(
        TYPEWRITER_MAX_CHARS_PER_TICK,
        Math.max(TYPEWRITER_MIN_CHARS_PER_TICK, Math.ceil(remaining / 48)),
      );
      const nextVisible = raw.slice(0, visible.length + charsThisTick);
      visibleStreamingTextRef.current = nextVisible;
      setVisibleStreamingText(nextVisible);
      if (autoFollowScrollRef.current) {
        window.setTimeout(scrollToBottom, 0);
      }

      if (nextVisible.length < streamingTextRef.current.length) {
        typewriterTimerRef.current = window.setTimeout(
          drain,
          TYPEWRITER_TICK_MS,
        );
      } else {
        const onDrained = typewriterOnDrainedRef.current;
        if (onDrained) {
          typewriterOnDrainedRef.current = null;
          typewriterTimerRef.current = window.setTimeout(() => {
            typewriterTimerRef.current = null;
            onDrained();
          }, 80);
        }
      }
    };

    typewriterTimerRef.current = window.setTimeout(drain, TYPEWRITER_TICK_MS);
  }, [scrollToBottom, shouldPreserveMessageSelection]);

  useEffect(() => {
    return () => {
      clearTypewriterTimer();
    };
  }, [clearTypewriterTimer]);

  const applyAdversarialRunEvent = useCallback(
    (event: ChatAdversarialStreamEvent) => {
      const nextStatus = normalizeAdversarialRunStatus(
        event.status,
        event.event,
      );
      setPmBackgroundTaskId(event.runId);
      setPmBackgroundTaskStatus(nextStatus);
      setPmPanelTaskId(event.runId);
      setPmPanelTaskStatus(nextStatus);
      setPmPanelOpen((open) => (superAssistantEndpoint ? open : true));
      setPmSuppressExecutionUi(false);
      const stageStatus = normalizeAdversarialStageStatus(nextStatus);
      const segmentId = ensurePmInlineSegment(
        "super_adversarial",
        stageStatus,
        1,
        describeAdversarialEvent(event),
        {
          kind: "chatAdversarialRun",
          event: event.event,
          runId: event.runId,
          threadId: event.threadId,
          round: event.round,
          model: event.model,
          status: nextStatus,
          error: event.error,
        },
      );
      if (!isAdversarialRunTerminalStatus(nextStatus)) {
        pmActiveInlineSegmentIdRef.current = segmentId;
      }
      const isReplyEvent =
        event.event.startsWith("model_") ||
        event.event.startsWith("judge_") ||
        event.event.startsWith("final_");
      const isReplyDelta = event.event.endsWith("_delta");
      if (isReplyEvent && !isReplyDelta) {
        const roleLabel = event.event.startsWith("judge_")
          ? "裁判模型"
          : event.event.startsWith("final_")
            ? "汇总模型"
            : "参与模型";
        const model = event.model?.trim() || "未知模型";
        const status: PmInlineAction["status"] = event.event.endsWith("_failed")
          ? "error"
          : event.event.endsWith("_completed")
            ? "success"
            : "running";
        const response = (event.text || event.error || "").trim();
        upsertPmInlineAction(event.messageId, event.round ?? 0, {
          name: model,
          source: "builtin",
          status,
          detail: response
            ? `${roleLabel} ${model}：${shortHumanText(response, 520)}`
            : `${roleLabel} ${model}${status === "running" ? "正在回复" : "已完成回复"}`,
        });
      }
      if (isAdversarialRunTerminalStatus(nextStatus)) {
        pmActiveInlineSegmentIdRef.current = null;
        setIsStreaming(false);
        onStreamingChange?.(false);
      }
    },
    [
      ensurePmInlineSegment,
      onStreamingChange,
      superAssistantEndpoint,
      upsertPmInlineAction,
    ],
  );

  const appendAdversarialTerminalMessageIfNeeded = useCallback(
    (run: ChatAdversarialRun, text: string) => {
      const finalText = text.trim();
      if (!finalText) return;
      if (streamingTextRef.current.trim()) {
        streamingTextRef.current = finalText;
        setStreamingText(finalText);
        flushVisibleStreamingText();
        resetStreamingText();
        streamCommittedRef.current = true;
      }
      setIsStreaming(false);
      onStreamingChange?.(false);
      const messageId = `adv-${run.id}`;
      const assistantMsg: DisplayMessage = {
        id: messageId,
        role: "assistant",
        content: finalText,
        timestamp: Date.now(),
        modelName:
          run.judge_model?.trim() ||
          activeResponseModelNameRef.current ||
          undefined,
        judgeModel: run.judge_model?.trim() || undefined,
        winnerModel: run.winner_model?.trim() || undefined,
        winnerReason: run.winner_reason?.trim() || undefined,
        adversarialRunId: run.id,
      };
      setDisplayMessages((prev) => {
        if (prev.some((msg) => msg.id === messageId)) return prev;
        return [...prev, assistantMsg];
      });
      setTimeout(scrollToBottom, 30);
    },
    [
      flushVisibleStreamingText,
      onStreamingChange,
      resetStreamingText,
      scrollToBottom,
    ],
  );

  const streamSuperAssistantAdversarialRun = useCallback(
    (runId: string) => {
      let finalText = "";
      return streamChatAdversarialRunEvents(runId, {
        onEvent: (event) => {
          markStreamActivity();
          if (event.event === "final_delta" && event.delta) {
            finalText += event.delta;
          } else if (event.event === "final_completed" && event.text) {
            finalText = event.text;
          } else if (
            event.event === "run_failed" &&
            event.error &&
            !finalText.trim()
          ) {
            finalText = event.error;
          }
          applyAdversarialRunEvent(event);
        },
        onEnd: () => {
          void agentApi
            .getChatAdversarialRun(runId)
            .then((run) => {
              const doneStatus = normalizeAdversarialRunStatus(run.status);
              setPmBackgroundTaskStatus(doneStatus);
              setPmPanelTaskStatus(doneStatus);
              pmBackgroundTaskAbortRef.current = null;
              superAssistantAsyncTaskStartedRef.current = false;
              setIsStreaming(false);
              onStreamingChange?.(false);
              const answer = (
                run.final_answer ||
                finalText ||
                run.error_message ||
                ""
              ).trim();
              appendAdversarialTerminalMessageIfNeeded(run, answer);
            })
            .catch((error) => {
              pmBackgroundTaskAbortRef.current = null;
              superAssistantAsyncTaskStartedRef.current = false;
              setPmBackgroundTaskStatus("failed");
              setPmPanelTaskStatus("failed");
              setIsStreaming(false);
              onStreamingChange?.(false);
              const text =
                error instanceof Error ? error.message : String(error);
              message.error(`${t("chat.streamError")}: ${text}`);
            });
        },
        onError: (err) => {
          setPmBackgroundTaskStatus("failed");
          setPmPanelTaskStatus("failed");
          pmBackgroundTaskAbortRef.current = null;
          superAssistantAsyncTaskStartedRef.current = false;
          setIsStreaming(false);
          onStreamingChange?.(false);
          message.error(`${t("chat.streamError")}: ${err}`);
        },
      });
    },
    [
      appendAdversarialTerminalMessageIfNeeded,
      applyAdversarialRunEvent,
      markStreamActivity,
      onStreamingChange,
      t,
    ],
  );

  // ── Queries ──────────────────────────────────────────────────────────────────────────────────
  const runtimeSessionSource = superAssistantEndpoint
    ? "super_assistant"
    : sessionSource;
  const { data: sessionsData, isLoading: sessionsLoading } = useQuery({
    queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
    queryFn: async () => {
      if (!superAssistantEndpoint)
        return agentApi.listSessions(runtimeSessionSource);
      const [unified, legacyPm] = await Promise.all([
        agentApi.listSessions("super_assistant"),
        agentApi.listSessions("pm"),
      ]);
      const byId = new Map(
        [...(legacyPm.sessions ?? []), ...(unified.sessions ?? [])].map(
          (session) => [session.session_id, session],
        ),
      );
      const sessions = Array.from(byId.values());
      return { sessions, total: sessions.length };
    },
    staleTime: 30_000,
  });

  const { data: commandsData } = useQuery({
    queryKey: queryKeys.commands.list(),
    queryFn: () => commandsApi.list(),
    staleTime: 5 * 60 * 1000,
  });

  const { data: mcpServersData } = useQuery({
    queryKey: queryKeys.mcp.list({ per_page: 200 }),
    queryFn: () => mcpApi.list({ per_page: 200 }),
    staleTime: 30_000,
  });

  const { data: skillsListData } = useQuery({
    queryKey: queryKeys.skills.list({ per_page: 200 }),
    queryFn: () => skillsApi.list({ per_page: 200 }),
    staleTime: 30_000,
  });

  useSystemEvents({
    autoConnect: true,
    onMcpUpdated: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.mcp.all });
      void qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
    },
    onSkillsUpdated: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.skills.all });
      void qc.invalidateQueries({ queryKey: queryKeys.commands.all });
      void qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
    },
  });

  const { data: chatCapabilities } = useQuery({
    queryKey: queryKeys.chatSessions.capabilities(requestedModel),
    queryFn: () => agentApi.getChatCapabilities(requestedModel),
    staleTime: 60_000,
    enabled: sessionSource === "chat",
  });

  const usesLegacyPmMemory = sessionSource === "pm" && !superAssistantEndpoint;
  const memoryQueryEnabled =
    superAssistantEndpoint ||
    (sessionSource === "chat" && !!chatCapabilities?.memory?.enabled) ||
    (usesLegacyPmMemory && !!activeSessionId);
  const {
    data: memoryPages,
    refetch: refetchMemoryItems,
    isLoading: memoryItemsLoading,
    isFetchingNextPage: memoryItemsFetchingNextPage,
    hasNextPage: memoryItemsHaveNextPage,
    fetchNextPage: fetchNextMemoryItemsPage,
  } = useInfiniteQuery({
    queryKey: [
      ...queryKeys.chatSessions.all,
      sessionSource,
      activeSessionId,
      "memories",
      memorySourceGroup,
    ],
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) =>
      agentApi.listUnifiedMemories({
        app: usesLegacyPmMemory ? "pm" : "chat",
        sessionId: activeSessionId ?? undefined,
        includeLegacy: usesLegacyPmMemory,
        sourceGroup: memorySourceGroup,
        cursor: pageParam,
        limit: 20,
      }),
    getNextPageParam: (lastPage) =>
      lastPage.hasMore ? (lastPage.nextCursor ?? undefined) : undefined,
    staleTime: 30_000,
    enabled: memoryQueryEnabled && memoryDrawerOpen,
  });
  const { data: memorySettings, refetch: refetchMemorySettings } = useQuery({
    queryKey: [
      ...queryKeys.chatSessions.all,
      sessionSource,
      activeSessionId,
      "memory-settings",
    ],
    queryFn: () =>
      usesLegacyPmMemory && activeSessionId
        ? agentApi.getPmSessionMemorySettings(activeSessionId).catch(() => null)
        : agentApi.getChatMemorySettings().catch(() => null),
    staleTime: 30_000,
    enabled: memoryQueryEnabled,
  });
  const visibleMemoryItems = useMemo(() => {
    const seen = new Set<string>();
    return (memoryPages?.pages ?? []).flatMap((page) =>
      page.items.flatMap((item) => {
        if (seen.has(item.id)) return [];
        seen.add(item.id);
        return [
          {
            id: item.id,
            sessionId: item.sessionId ?? undefined,
            memoryType: item.memoryType,
            content: item.content,
            source: item.sourceType,
            confidence: item.confidence,
            pinned: item.pinned,
            enabled: item.enabled,
            metadata: item.metadata,
            createdAt: item.createdAt,
            updatedAt: item.updatedAt,
            legacySource: item.legacySource,
          },
        ];
      }),
    );
  }, [memoryPages?.pages]);

  const {
    data: contextStatus,
    refetch: refetchContextStatus,
    isFetching: contextStatusFetching,
  } = useQuery({
    queryKey: [
      ...queryKeys.agentSessions.detail(activeSessionId ?? "none"),
      "context-status",
    ],
    queryFn: () => agentApi.getSessionContextStatus(activeSessionId!),
    staleTime: 20_000,
    enabled: !!activeSessionId && memoryDrawerOpen,
  });

  const searchCapability = chatCapabilities?.search;
  const webSearchAvailable =
    sessionSource === "chat" && !!searchCapability?.enabled;
  const attachedDocumentCount = attachments.filter(
    (att) => att.type === "document",
  ).length;
  const memoryPaused =
    contextStatus?.memoryState?.useMemories === false ||
    !!memorySettings?.paused;
  const memoryPollutionReasonLabel = useMemo(() => {
    const reason = contextStatus?.memoryState?.pollutionReason;
    if (!reason) return null;
    const labels: Record<string, string> = {
      web_search_context: t(
        "chat.memoryPollutionReasonWebSearch",
        "web search context",
      ),
      file_workspace_context: t(
        "chat.memoryPollutionReasonFileWorkspace",
        "file workspace context",
      ),
      pm_document_context: t(
        "chat.memoryPollutionReasonPmDocument",
        "PM document context",
      ),
      pm_image_context: t(
        "chat.memoryPollutionReasonPmImage",
        "PM image context",
      ),
      external_context: t(
        "chat.memoryPollutionReasonExternal",
        "external context",
      ),
    };
    return labels[reason] ?? reason.replace(/_/g, " ");
  }, [contextStatus?.memoryState?.pollutionReason, t]);
  const memoryTypeLabel = useCallback(
    (type?: string | null) => {
      const normalized = (type ?? "").toLowerCase();
      const labels: Record<string, string> = {
        preference: t("chat.memoryTypePreference", "Preference"),
        project_fact: t("chat.memoryTypeProjectFact", "Project fact"),
        business_context: t(
          "chat.memoryTypeBusinessContext",
          "Business context",
        ),
        analysis_style: t("chat.memoryTypeAnalysisStyle", "Analysis style"),
        workflow: t("chat.memoryTypeWorkflow", "Workflow"),
        pitfall: t("chat.memoryTypePitfall", "Pitfall"),
        decision: t("chat.memoryTypeDecision", "Decision"),
        note: t("chat.memoryTypeNote", "Note"),
      };
      return (
        labels[normalized] ??
        (normalized.replace(/_/g, " ") || t("chat.memoryTypeNote", "Note"))
      );
    },
    [t],
  );
  const memoryEnabled =
    superAssistantEndpoint ||
    (sessionSource === "chat" && !!chatCapabilities?.memory?.enabled) ||
    (usesLegacyPmMemory && !!activeSessionId);
  useEffect(() => {
    if (!webSearchAvailable && searchMode === "on") {
      setSearchMode("off");
    }
  }, [webSearchAvailable, searchMode]);

  const refreshChatMemories = useCallback(() => {
    void refetchMemoryItems();
    void refetchMemorySettings();
    qc.invalidateQueries({
      queryKey: [
        ...queryKeys.chatSessions.all,
        sessionSource,
        activeSessionId,
        "memories",
      ],
    });
  }, [
    activeSessionId,
    qc,
    refetchMemoryItems,
    refetchMemorySettings,
    sessionSource,
  ]);

  const handleCreateMemory = useCallback(async () => {
    const content = memoryDraft.trim();
    if (!content) return;
    setMemoryCreating(true);
    try {
      if (usesLegacyPmMemory && activeSessionId) {
        await agentApi.createUnifiedMemory({
          app: "pm",
          scope: "session",
          sessionId: activeSessionId,
          content,
          memoryType: "project_fact",
          sourceType: "manual",
          pinned: false,
          enabled: true,
        });
      } else {
        await agentApi.createUnifiedMemory({
          app: "chat",
          scope: activeSessionId ? "session" : "app",
          sessionId: activeSessionId ?? undefined,
          content,
          memoryType: "project_fact",
          sourceType: "manual",
          pinned: false,
          enabled: true,
        });
      }
      setMemoryDraft("");
      setMemorySourceGroup("manual");
      refreshChatMemories();
      message.success(t("chat.memorySaved", "Memory saved"));
    } catch (error) {
      message.error(
        `${t("chat.memorySaveFailed", "Failed to save memory")}: ${(error as Error).message}`,
      );
    } finally {
      setMemoryCreating(false);
    }
  }, [
    activeSessionId,
    memoryDraft,
    refreshChatMemories,
    t,
    usesLegacyPmMemory,
  ]);

  const handleToggleMemoryPause = useCallback(async () => {
    setMemoryModeUpdating(true);
    try {
      if (activeSessionId) {
        await agentApi.updateSessionMemoryMode(activeSessionId, {
          useMemories: memoryPaused,
          generateMemories: memoryPaused,
          pollutionState: memoryPaused ? "clean" : undefined,
        });
      } else {
        await agentApi.pauseChatMemory(!memoryPaused);
      }
      refreshChatMemories();
      void refetchContextStatus();
    } catch (error) {
      message.error(
        `${t("chat.memoryUpdateFailed", "Failed to update memory")}: ${(error as Error).message}`,
      );
    } finally {
      setMemoryModeUpdating(false);
    }
  }, [
    activeSessionId,
    memoryPaused,
    refreshChatMemories,
    refetchContextStatus,
    t,
  ]);

  const handleDeleteMemory = useCallback(
    async (id: string) => {
      setMemoryDeletingId(id);
      try {
        if (
          id.startsWith("legacy-pm-") &&
          usesLegacyPmMemory &&
          activeSessionId
        ) {
          await agentApi.deletePmSessionMemory(
            activeSessionId,
            id.replace(/^legacy-pm-/, ""),
          );
        } else if (id.startsWith("legacy-chat-")) {
          await agentApi.deleteChatMemory(id.replace(/^legacy-chat-/, ""));
        } else {
          await agentApi.deleteUnifiedMemory(id);
        }
        refreshChatMemories();
      } catch (error) {
        message.error(
          `${t("chat.memoryDeleteFailed", "Failed to delete memory")}: ${(error as Error).message}`,
        );
      } finally {
        setMemoryDeletingId(null);
      }
    },
    [activeSessionId, refreshChatMemories, t, usesLegacyPmMemory],
  );

  const handleManualCompact =
    useCallback(async (): Promise<AgentManualCompactionResult | null> => {
      if (!activeSessionId) {
        message.info(
          t("chat.contextCompactNoSession", "No active session to compact."),
        );
        return null;
      }
      setContextCompacting(true);
      try {
        const result = await agentApi.compactSessionContext(activeSessionId);
        setLastManualCompaction(result);
        message.success(
          t(
            "chat.contextCompactSuccess",
            "Context compacted: {{count}} messages summarized",
            {
              count: result.removedMessageCount,
            },
          ),
        );
        void refetchContextStatus();
        return result;
      } catch (error) {
        message.error(
          `${t("chat.contextCompactFailed", "Failed to compact context")}: ${(error as Error).message}`,
        );
        return null;
      } finally {
        setContextCompacting(false);
      }
    }, [activeSessionId, refetchContextStatus, t]);

  const sessions: SessionItem[] = useMemo(() => {
    return (sessionsData?.sessions ?? []).map((s: any) => ({
      sessionId: s.session_id,
      name: s.name || s.session_id.slice(0, 12),
      state: s.state,
      model: s.model,
      createdAt: s.created_at,
      lastActivity: s.last_activity,
      source: s.source,
      mcpServers: sessionMetadataNames(s.mcp_servers),
      skills: sessionMetadataNames(s.skills),
      permissionMode:
        typeof s.permission_mode === "string" ? s.permission_mode : undefined,
      isPinned: s.is_pinned ?? false,
      isBookmarked: s.is_bookmarked ?? false,
      projectIds: s.project_ids,
    }));
  }, [sessionsData]);

  const selectedSession = useMemo(
    () => sessions.find((session) => session.sessionId === activeSessionId),
    [activeSessionId, sessions],
  );

  useEffect(() => {
    if (!isStreaming) {
      rememberResponseModel(selectedSession?.model || requestedModel);
    }
  }, [
    isStreaming,
    rememberResponseModel,
    requestedModel,
    selectedSession?.model,
  ]);

  const visibleAssistantModelName = superAssistantEndpoint
    ? activeResponseModelName ||
      selectedSession?.model ||
      requestedModel ||
      pmAssistantModelName
    : sessionSource === "pm"
      ? pmAssistantModelName
      : requestedModel || selectedSession?.model;

  const configuredMcpServers = useMemo(
    () => enabledMcpServerNames(mcpServersData),
    [mcpServersData],
  );

  const configuredSkills = useMemo(() => {
    const fromSkillsApi = enabledSkillNamesFromList(skillsListData);
    if (fromSkillsApi.length > 0) return fromSkillsApi;
    return uniqueNonEmptyStrings(
      (commandsData?.skills ?? []).map((command: any) => command?.name),
    );
  }, [commandsData, skillsListData]);

  const effectiveMcpServers = useMemo(
    () =>
      uniqueNonEmptyStrings([
        ...activeMcpServers,
        ...(selectedSession?.mcpServers ?? []),
        ...configuredMcpServers,
      ]),
    [activeMcpServers, configuredMcpServers, selectedSession?.mcpServers],
  );

  const effectiveSkills = useMemo(
    () =>
      uniqueNonEmptyStrings([
        ...activeSkills,
        ...(selectedSession?.skills ?? []),
        ...configuredSkills,
      ]),
    [activeSkills, configuredSkills, selectedSession?.skills],
  );

  useEffect(() => {
    if (!selectedSession) return;
    if (selectedSession.mcpServers) {
      setActiveMcpServers(selectedSession.mcpServers);
    }
    if (selectedSession.skills) {
      setActiveSkills(selectedSession.skills);
    }
  }, [selectedSession]);

  const allSlashCommands: SlashCommandDef[] = useMemo(() => {
    const superAssistantCommands: SlashCommandDef[] = superAssistantEndpoint
      ? [
          {
            name: t("superAssistant.slashDataAttributionName", "数据归因"),
            description: t(
              "superAssistant.slashDataAttributionDescription",
              "本次提问只走数据归因，自动查数、对比、诊断并输出结论。",
            ),
            hint: t(
              "superAssistant.slashDataAttributionHint",
              "/数据归因 昨天 ROI 为什么下降？",
            ),
            source: "builtin" as const,
            category: "super_assistant",
          },
          {
            name: t("superAssistant.slashDeepResearchName", "深度研究"),
            description: t(
              "superAssistant.slashDeepResearchDescription",
              "本次提问执行深度研究，持续检索、核验来源并综合结论。",
            ),
            hint: t(
              "superAssistant.slashDeepResearchHint",
              "/深度研究 调研 AI Agent 的行业实践与关键差异",
            ),
            source: "builtin" as const,
            category: "super_assistant",
          },
          {
            name: t("superAssistant.slashSuperAdversarialName", "超级对抗"),
            description: t(
              "superAssistant.slashSuperAdversarialDescription",
              "本次提问由多个模型独立分析、交叉质疑并裁决结论。",
            ),
            hint: t(
              "superAssistant.slashSuperAdversarialHint",
              "/超级对抗 比较两个方案并给出最终取舍",
            ),
            source: "builtin" as const,
            category: "super_assistant",
          },
        ]
      : [];
    if (!commandsData) return superAssistantCommands;
    const builtin: SlashCommandDef[] = (commandsData.builtin ?? [])
      .filter((command: any) =>
        WEB_BUILTIN_SLASH_COMMANDS.has(
          String(command?.name ?? "").toLowerCase(),
        ),
      )
      .map((c: any) => ({ ...c, source: "builtin" as const }));
    if (!builtin.some((command) => command.name.toLowerCase() === "commands")) {
      builtin.unshift({
        name: "commands",
        description: t(
          "chat.slashCommandsDescription",
          "Open the slash command palette",
        ),
        source: "builtin",
      });
    }
    const skills: SlashCommandDef[] = (commandsData.skills ?? []).map(
      (c: any) => ({
        name: c.name,
        description: c.description,
        hint: c.hint,
        source: "skill" as const,
      }),
    );
    return [...superAssistantCommands, ...builtin, ...skills];
  }, [commandsData, superAssistantEndpoint, t]);

  // ── File upload ──────────────────────────────────────────────────────────────────────────────
  const uploadFiles = useCallback(
    async (files: FileList | File[]) => {
      setUploading(true);
      let pmImageCount = attachments.filter(
        (att) => att.type === "image",
      ).length;
      let pmLimitWarned = false;
      for (const file of Array.from(files)) {
        try {
          if (
            (sessionSource === "chat" || sessionSource === "pm") &&
            isUnsupportedDocumentUpload(file)
          ) {
            message.warning(
              t(
                "chat.pdfUploadUnsupported",
                "This file type is temporarily disabled for file Q&A. Please upload txt, md, csv, json, html, docx, or xlsx.",
              ),
            );
            continue;
          }
          const uploaded = await uploadFile(file);
          const isImage = uploaded.mediaType.startsWith("image/");
          if (
            sessionSource === "pm" &&
            isImage &&
            pmImageCount >= PM_MAX_IMAGE_ATTACHMENTS
          ) {
            if (!pmLimitWarned) {
              pmLimitWarned = true;
              message.warning(
                t(
                  "operations.pmBackgroundImageLimit",
                  "后台研究最多支持 5 张图片，请减少附件后重试。",
                ),
              );
            }
            continue;
          }
          const previewUrl = isImage ? URL.createObjectURL(file) : undefined;
          const block: ContentBlock = isImage
            ? {
                type: "image",
                fileId: uploaded.fileId,
                media_type: uploaded.mediaType,
                sourceType: "url",
                data: uploaded.url,
                name: uploaded.filename,
                sizeBytes: uploaded.size,
                previewUrl,
              }
            : {
                type: "document",
                fileId: uploaded.fileId,
                media_type: uploaded.mediaType,
                sourceType: "url",
                data: uploaded.url,
                name: uploaded.filename,
                sizeBytes: uploaded.size,
              };
          setAttachments((prev) => [...prev, block]);
          if (sessionSource === "chat" || sessionSource === "pm") {
            void agentApi
              .registerChatFile({
                fileId: uploaded.fileId,
                filename: uploaded.filename,
                mediaType: uploaded.mediaType,
                size: uploaded.size,
                url: uploaded.url,
                sessionId: activeSessionId,
              })
              .then((record) => {
                setChatFileRecords((prev) => ({
                  ...prev,
                  [record.fileId]: record,
                }));
                if (record.status === "failed") {
                  message.warning(
                    `${t("chat.fileIndexFailed", "File indexing failed")}: ${record.errorMessage ?? record.filename}`,
                  );
                }
              })
              .catch((error) => {
                message.warning(
                  `${t("chat.fileIndexFailed", "File indexing failed")}: ${(error as Error).message}`,
                );
              });
          }
          if (sessionSource === "pm" && isImage) {
            pmImageCount += 1;
          }
        } catch (err) {
          message.error(`${t("chat.uploadFailed")}: ${(err as Error).message}`);
        }
      }
      setUploading(false);
    },
    [activeSessionId, attachments, sessionSource, t],
  );

  const handleDrop = useCallback(
    async (e: React.DragEvent) => {
      e.preventDefault();
      setDraggingOver(false);
      if (e.dataTransfer.files?.length) {
        await uploadFiles(e.dataTransfer.files);
      }
    },
    [uploadFiles],
  );

  const handlePaste = useCallback(
    (event: React.ClipboardEvent<HTMLTextAreaElement>) => {
      if (uploading || isStreaming) return;
      if (event.clipboardData.files.length > 0) {
        event.preventDefault();
        void uploadFiles(event.clipboardData.files);
        return;
      }
      const text = event.clipboardData.getData("text/plain");
      if (!shouldAttachPastedText(text)) return;
      event.preventDefault();
      const file = new File([text], pastedTextFileName(text), {
        type: pastedTextLooksLikeSql(text) ? "application/sql" : "text/plain",
      });
      void uploadFiles([file]);
    },
    [isStreaming, uploadFiles, uploading],
  );

  // ── Session history ──────────────────────────────────────────────────────────────────────────
  const loadSessionMessages = useCallback(
    async (sessionId: string | null) => {
      if (!sessionId) return;
      if (superAssistantEndpoint) {
        superAssistantTurnIdRef.current = null;
      }
      const loadToken = `load-${sessionId}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      latestRequestRef.current = loadToken;
      if (abortRef.current) {
        if (superAssistantEndpoint) {
          abortRef.current();
          abortRef.current = null;
        } else {
          stopActiveTurn(activeSessionId);
        }
      }
      setIsStreaming(false);
      resetStreamingText();
      setToolCalls({});
      toolCallsRef.current = {};
      liveToolIndicesRef.current = new Set();
      liveToolKeyByIndexRef.current = {};
      resetThinkingState();
      setDisplayMessages([]);
      setHistoryHasMore(false);
      setHistoryBeforeTurnCursor(null);
      setHistoryLoadingMore(false);
      resetPmResearchState();
      clearPmPromptQueue();

      try {
        const [res, memoryCitationsResp] = await Promise.all([
          agentApi.getSessionHistory(sessionId, {
            limit_turns: HISTORY_PAGE_LIMIT_TURNS,
            max_bytes: HISTORY_PAGE_MAX_BYTES,
          }),
          sessionSource === "chat" ||
          sessionSource === "agent" ||
          sessionSource === "pm"
            ? agentApi.listSessionMemoryCitations(sessionId).catch(() => ({
                sessionId,
                items: [] as AgentMemoryCitation[],
              }))
            : Promise.resolve({
                sessionId,
                items: [] as AgentMemoryCitation[],
              }),
        ]);
        if (latestRequestRef.current !== loadToken) return;
        let merged = attachSuperAssistantTurnMetadata(
          mapHistoryMessages(res.messages, {
            source: runtimeSessionSource,
            idPrefix: `hist-${sessionId}-${res.page?.before_turn_cursor ?? "latest"}`,
          }),
          res.super_assistant_turns,
        );
        if (sessionSource === "pm" && res.pm_research) {
          merged = attachPmFinalDeliveryArtifacts(
            merged,
            res.pm_research.delivery_artifacts,
          );
        }
        merged = attachMemoryCitationsToMessages(
          merged,
          memoryCitationsResp.items ?? [],
        );
        void registerHistoricalImagesForSession(merged, sessionId).then(
          (records) => {
            if (
              latestRequestRef.current !== loadToken ||
              Object.keys(records).length === 0
            )
              return;
            setChatFileRecords((prev) => ({ ...prev, ...records }));
          },
        );
        if (sessionSource === "pm" && res.pm_research) {
          const latestDeliveryArtifact = Array.isArray(
            res.pm_research.delivery_artifacts,
          )
            ? res.pm_research.delivery_artifacts.find(
                (artifact) => artifact.taskId === res.pm_research?.task_id,
              )
            : undefined;
          const durableStageEvents = Array.isArray(
            latestDeliveryArtifact?.stages,
          )
            ? latestDeliveryArtifact.stages
                .filter((raw) => {
                  const stage = String(raw.stage ?? "").toLowerCase();
                  return !["done", "failed", "cancelled"].includes(stage);
                })
                .map((raw) => ({
                  task_id: res.pm_research!.task_id,
                  session_id: sessionId,
                  status: String(raw.status ?? "completed"),
                  stage: String(raw.stage ?? ""),
                  attempt:
                    typeof raw.attempt === "number" ? raw.attempt : 1,
                  elapsed_ms: 0,
                  detail:
                    raw.detail && typeof raw.detail === "object"
                      ? (raw.detail as Record<string, unknown>)
                      : undefined,
                }))
            : [];
          const replayEvents = Array.isArray(res.pm_research.events)
            ? res.pm_research.events
            : [];
          const replayEventsWithProjection = [
            ...replayEvents,
            ...durableStageEvents,
          ];
          const normalizedReplayDetails = replayEventsWithProjection.map((event) => {
            const detail = normalizePmTaskEventDetail(
              event as ApiPmResearchTaskEvent,
            );
            return {
              stage: normalizePmTaskEventStage(event as ApiPmResearchTaskEvent),
              status: normalizePmTaskEventStageStatus(
                event as ApiPmResearchTaskEvent,
              ),
              attempt: event.attempt ?? 1,
              detail,
            };
          });
          const replaySearchUsage = buildPmSearchUsageFromEvents(
            normalizedReplayDetails,
          );
          const replayLooksLikeLightChat = replayEventsWithProjection.some((event) => {
            const detail =
              event.detail &&
              typeof event.detail === "object" &&
              !Array.isArray(event.detail)
                ? (event.detail as Record<string, unknown>)
                : undefined;
            return isPmLightweightChatDetail(detail);
          });
          setPmSuppressExecutionUi(replayLooksLikeLightChat);
          const terminalReplayEvent = [...replayEvents]
            .reverse()
            .find((event) =>
              isPmTaskTerminalEvent(event as ApiPmResearchTaskEvent),
            );
          const replayQualityEvent = [...replayEvents]
            .reverse()
            .find((event) => {
              const response =
                event.response &&
                typeof event.response === "object" &&
                !Array.isArray(event.response)
                  ? (event.response as Record<string, unknown>)
                  : undefined;
              return !!normalizePmQualitySnapshot(response?.pm_quality);
            });
          const terminalQuality = terminalReplayEvent
            ? normalizePmQualitySnapshot(
                (
                  terminalReplayEvent.response as
                    Record<string, unknown> | undefined
                )?.pm_quality,
              )
            : undefined;
          const replayQuality =
            terminalQuality ??
            (replayQualityEvent
              ? normalizePmQualitySnapshot(
                  (replayQualityEvent.response as Record<string, unknown>)
                    .pm_quality,
                )
              : undefined);
          if (replayQuality) {
            setPmQualitySnapshot(replayQuality);
            pmQualitySnapshotRef.current = replayQuality;
          }
          if (terminalReplayEvent) {
            const terminalText = resolvePmTerminalMessageText(
              terminalReplayEvent as ApiPmResearchTaskEvent,
              t("chat.unknownError", "未知错误"),
            );
            const terminalReport = normalizePmReportArtifact(
              (
                terminalReplayEvent.response as
                  Record<string, unknown> | undefined
              )?.pm_report,
            );
            if (terminalText) {
              merged = reconcilePmHistoryTerminalAssistant(
                merged,
                terminalText,
                {
                  taskId: terminalReplayEvent.task_id,
                  taskStatus: terminalReplayEvent.status,
                  pmReport: terminalReport,
                  preserveRicherContent: true,
                },
              );
            }
          }
          const historyTaskStatus = (
            res.pm_research.status ?? ""
          ).toLowerCase();
          const effectiveTaskStatus = terminalReplayEvent
            ? derivePmBackgroundTaskStatus(
                terminalReplayEvent as ApiPmResearchTaskEvent,
              )
            : historyTaskStatus;

          setPmBackgroundTaskId(res.pm_research.task_id);
          setPmBackgroundTaskStatus(
            effectiveTaskStatus && effectiveTaskStatus.length > 0
              ? effectiveTaskStatus
              : null,
          );
          setPmPanelTaskId(res.pm_research.task_id);
          setPmPanelTaskStatus(
            effectiveTaskStatus && effectiveTaskStatus.length > 0
              ? effectiveTaskStatus
              : null,
          );
          if (replayEventsWithProjection.length > 0) {
            const replayBase = Date.now();
            const restoredStates: Record<string, PmStageState> = {};
            const restoredEvents: PmStageEvent[] = [];
            replayEventsWithProjection.forEach((event, idx) => {
              const stageName = normalizePmTaskEventStage(
                event as ApiPmResearchTaskEvent,
              );
              const status = normalizePmTaskEventStageStatus(
                event as ApiPmResearchTaskEvent,
              );
              const attempt =
                typeof event.attempt === "number" && event.attempt > 0
                  ? event.attempt
                  : 1;
              const at = replayBase + idx;
              const rawDetail = normalizePmTaskEventDetail(
                event as ApiPmResearchTaskEvent,
              );
              restoredStates[stageName] = {
                stage: stageName,
                status,
                attempt,
                detail: rawDetail,
                updatedAt: at,
              };
              restoredEvents.push({
                stage: stageName,
                status,
                attempt,
                detail: rawDetail,
                at,
              });
              const summary = buildPmStageNarrative(
                stageName,
                status,
                rawDetail,
              );
              ensurePmInlineSegment(
                stageName,
                status,
                attempt,
                summary,
                rawDetail,
              );
            });
            const replayTerminal =
              !!terminalReplayEvent ||
              isPmTaskTerminalStatus(effectiveTaskStatus) ||
              isPmTaskTerminalStatus(historyTaskStatus);
            const normalizedStates = replayTerminal
              ? Object.fromEntries(
                  Object.entries(restoredStates).map(([key, value]) => [
                    key,
                    value.status === "running"
                      ? {
                          ...value,
                          status: "completed" as const,
                        }
                      : value,
                  ]),
                )
              : restoredStates;
            setPmStageStates(normalizedStates);
            pmStageStatesRef.current = normalizedStates;
            const tailEvents = restoredEvents.slice(-120);
            setPmStageEvents(tailEvents);
            pmStageEventsRef.current = tailEvents;
            pmActiveInlineSegmentIdRef.current = null;
          }
          merged = attachPmSearchUsageToLatestAssistant(
            merged,
            replaySearchUsage,
            normalizedReplayDetails.slice(-40),
          );
        }
        if (latestRequestRef.current !== loadToken) return;
        setDisplayMessages(merged);
        autoFollowScrollRef.current = true;
        if (sessionSource === "chat") {
          void Promise.all([
            agentApi.getChatSessionEvidence(sessionId),
            agentApi.getChatSessionTrace(sessionId),
          ])
            .then(([evidence, trace]) => {
              if (latestRequestRef.current !== loadToken) return;
              setDisplayMessages((prev) =>
                attachChatArtifactsToLatestAssistant(
                  prev,
                  evidence.items ?? [],
                  trace.items ?? [],
                ),
              );
            })
            .catch(() => {
              // Artifacts are best-effort; chat history remains usable without them.
            });
        }
        const nextBefore = res.page?.next_before_turn_cursor;
        setHistoryBeforeTurnCursor(
          typeof nextBefore === "number" ? nextBefore : null,
        );
        setHistoryHasMore(
          res.page?.has_more === true && typeof nextBefore === "number",
        );
        if (sessionSource === "pm") {
          const latestAssistant = [...merged]
            .reverse()
            .find((row) => row.role === "assistant");
          if (latestAssistant && !pmQualitySnapshotRef.current) {
            const answerText = contentToPlain(latestAssistant.content);
            const restoredQuality = buildPmQualitySnapshotFromHistory(
              answerText,
              latestAssistant.toolCalls,
            );
            setPmQualitySnapshot(restoredQuality);
            pmQualitySnapshotRef.current = restoredQuality;
          }
        }
        setActiveSessionId(sessionId);
        focusInputAndScrollToBottom();
        if (superAssistantEndpoint) {
          void agentApi
            .getSuperAssistantActiveTurn(sessionId)
            .then((activeTurn) => {
              if (latestRequestRef.current !== loadToken) return;
              if (!activeTurn.active) return;
              superAssistantTurnIdRef.current = activeTurn.turnId ?? null;
              if (activeTurn.link === "pmResearchTask" && activeTurn.taskId) {
                if (pmBackgroundTaskAbortRef.current) {
                  pmBackgroundTaskAbortRef.current();
                  pmBackgroundTaskAbortRef.current = null;
                }
                resetPmResearchState();
                setPmBackgroundTaskId(activeTurn.taskId);
                setPmBackgroundTaskStatus(activeTurn.status || "running");
                setPmPanelTaskId(activeTurn.taskId);
                setPmPanelTaskStatus(activeTurn.status || "running");
                markStreamActivity();
                setStreamingMessageTimestamp(Date.now());
                setIsStreaming(true);
                onStreamingChange?.(true);
                pmBackgroundTaskAbortRef.current = streamPmResearchTask(
                  activeTurn.taskId,
                  {
                    onEvent: (event) => {
                      applyPmTaskEvent(event);
                    },
                    onAnswerDelta: (event) => {
                      appendPmAnswerDelta(event.delta);
                    },
                    onImageContextWarning: handlePmImageContextWarning,
                    onDone: (event) => {
                      applyPmTaskEvent(event);
                      flushVisibleStreamingText();
                      const doneStatus = derivePmBackgroundTaskStatus(event);
                      setPmBackgroundTaskStatus(doneStatus);
                      setPmPanelTaskStatus(doneStatus);
                      pmBackgroundTaskAbortRef.current = null;
                      setIsStreaming(false);
                      onStreamingChange?.(false);
                      appendPmTerminalMessageIfNeeded(event);
                    },
                    onError: (err) => {
                      setPmBackgroundTaskStatus("failed");
                      setPmPanelTaskStatus("failed");
                      pmBackgroundTaskAbortRef.current = null;
                      setIsStreaming(false);
                      onStreamingChange?.(false);
                      message.error(`${t("chat.streamError")}: ${err}`);
                    },
                  },
                );
                return;
              }
              if (
                activeTurn.link === "chatAdversarialRun" &&
                activeTurn.taskId
              ) {
                if (pmBackgroundTaskAbortRef.current) {
                  pmBackgroundTaskAbortRef.current();
                  pmBackgroundTaskAbortRef.current = null;
                }
                resetPmResearchState();
                setPmBackgroundTaskId(activeTurn.taskId);
                setPmBackgroundTaskStatus(activeTurn.status || "running");
                setPmPanelTaskId(activeTurn.taskId);
                setPmPanelTaskStatus(activeTurn.status || "running");
                markStreamActivity();
                setStreamingMessageTimestamp(Date.now());
                setIsStreaming(true);
                onStreamingChange?.(true);
                pmBackgroundTaskAbortRef.current =
                  streamSuperAssistantAdversarialRun(activeTurn.taskId);
                return;
              }
              if (
                activeTurn.link === "dataAttributionTask" &&
                activeTurn.taskId
              ) {
                if (pmBackgroundTaskAbortRef.current) {
                  pmBackgroundTaskAbortRef.current();
                  pmBackgroundTaskAbortRef.current = null;
                }
                resetPmResearchState();
                setPmBackgroundTaskId(activeTurn.taskId);
                setPmBackgroundTaskStatus(activeTurn.status || "running");
                setPmPanelTaskId(activeTurn.taskId);
                setPmPanelTaskStatus(activeTurn.status || "running");
                markStreamActivity();
                setStreamingMessageTimestamp(Date.now());
                setIsStreaming(true);
                onStreamingChange?.(true);
                pmBackgroundTaskAbortRef.current = streamNl2sqlAttributionTask(
                  activeTurn.taskId,
                  {
                    onEvent: (event) => {
                      applyAttributionTaskEvent(event);
                    },
                    onDone: (event) => {
                      applyAttributionTaskEvent(event);
                      pmBackgroundTaskAbortRef.current = null;
                      const doneStatus = normalizeAttributionTaskStatus(
                        event.status,
                      );
                      setPmBackgroundTaskStatus(doneStatus);
                      setPmPanelTaskStatus(doneStatus);
                      setIsStreaming(false);
                      onStreamingChange?.(false);
                      appendAttributionTerminalMessageIfNeeded(event);
                    },
                    onError: (err) => {
                      setPmBackgroundTaskStatus("failed");
                      setPmPanelTaskStatus("failed");
                      pmBackgroundTaskAbortRef.current = null;
                      setIsStreaming(false);
                      onStreamingChange?.(false);
                      message.error(`${t("chat.streamError")}: ${err}`);
                    },
                  },
                );
                return;
              }
              if (!activeTurn.turnId) return;
              abortRef.current?.();
              abortRef.current = null;
              streamCommittedRef.current = false;
              resetStreamingText();
              setToolCalls({});
              toolCallsRef.current = {};
              liveToolIndicesRef.current = new Set();
              liveToolKeyByIndexRef.current = {};
              resetThinkingState();
              setIsStreaming(true);
              onStreamingChange?.(true);
              markStreamActivity();
              thinkingLoadingRef.current = true;
              setThinkingLoading(true);
              const turnStartedAtMs = Date.now();
              setStreamingMessageTimestamp(turnStartedAtMs);
              abortRef.current = streamSuperAssistantTurnEvents(
                activeTurn.turnId,
                {
                  onSuperAssistantTurnId: (turnId) => {
                    superAssistantTurnIdRef.current = turnId;
                  },
                  onSessionActivated: (meta: any) => {
                    if (meta.mcp_servers) setActiveMcpServers(meta.mcp_servers);
                    if (meta.skills) setActiveSkills(meta.skills);
                    rememberResponseModel(meta.model);
                  },
                  onConfigHotReload: (meta: any) => {
                    if (meta.mcp_servers) setActiveMcpServers(meta.mcp_servers);
                    if (meta.skills) setActiveSkills(meta.skills);
                    rememberResponseModel(meta.model);
                  },
                  onPmStage: applySuperAssistantPmStage,
                  onThinkingStart: () => {
                    markStreamActivity();
                    syntheticThinkingHintRef.current = false;
                    thinkingStartedAtRef.current = Date.now();
                    thinkingDurationRef.current = undefined;
                    setThinkingDurationMs(undefined);
                    setTimeout(scrollToBottom, 10);
                  },
                  onThinkingDelta: (text: string) => {
                    markStreamActivity();
                    if (syntheticThinkingHintRef.current) {
                      thinkingTextRef.current = "";
                      setThinkingText("");
                      syntheticThinkingHintRef.current = false;
                    }
                    thinkingLoadingRef.current = false;
                    setThinkingLoading(false);
                    thinkingTextRef.current += text;
                    setThinkingText(thinkingTextRef.current);
                    setTimeout(scrollToBottom, 10);
                  },
                  onThinkingEnd: () => {
                    markStreamActivity();
                    if (thinkingStartedAtRef.current != null) {
                      const dur = Date.now() - thinkingStartedAtRef.current;
                      thinkingDurationRef.current = dur;
                      setThinkingDurationMs(dur);
                      thinkingStartedAtRef.current = null;
                    }
                    thinkingLoadingRef.current = false;
                    setThinkingLoading(false);
                  },
                  onTextBlockStart: () => {
                    markStreamActivity();
                    if (thinkingStartedAtRef.current != null) {
                      const dur = Date.now() - thinkingStartedAtRef.current;
                      thinkingDurationRef.current = dur;
                      setThinkingDurationMs(dur);
                      thinkingStartedAtRef.current = null;
                    }
                    thinkingLoadingRef.current = false;
                    setThinkingLoading(false);
                  },
                  onText: (text: string) => {
                    markStreamActivity();
                    streamingTextRef.current += text;
                    setStreamingText(streamingTextRef.current);
                    scheduleTypewriterDrain();
                  },
                  onToolUseStart: (index: number, id: string, name: string) => {
                    markStreamActivity();
                    const parsed = parseToolName(name);
                    const toolKey = id
                      ? `resume:${id}`
                      : `resume:${index}:${Date.now()}`;
                    liveToolKeyByIndexRef.current[index] = toolKey;
                    toolCallsRef.current = {
                      ...toolCallsRef.current,
                      [toolKey]: {
                        index,
                        name: parsed.tool,
                        source: parsed.source,
                        mcpServer:
                          parsed.source === "mcp"
                            ? parsed.sourceName
                            : undefined,
                        skillName:
                          parsed.source === "skill"
                            ? parsed.sourceName
                            : undefined,
                        args: "",
                        result: "",
                        isError: false,
                        status: "pending",
                      },
                    };
                    liveToolIndicesRef.current.add(toolKey);
                    setToolCalls({ ...toolCallsRef.current });
                  },
                  onToolInputDelta: (index: number, partialJson: string) => {
                    markStreamActivity();
                    const toolKey = liveToolKeyByIndexRef.current[index];
                    if (!toolKey || !toolCallsRef.current[toolKey]) return;
                    const existing = toolCallsRef.current[toolKey];
                    toolCallsRef.current = {
                      ...toolCallsRef.current,
                      [toolKey]: {
                        ...existing,
                        args: mergeToolInput(existing.args, partialJson),
                        status: "running",
                      },
                    };
                    setToolCalls({ ...toolCallsRef.current });
                  },
                  onToolUseEnd: (index: number) => {
                    markStreamActivity();
                    const toolKey = liveToolKeyByIndexRef.current[index];
                    if (!toolKey || !toolCallsRef.current[toolKey]) return;
                    toolCallsRef.current = {
                      ...toolCallsRef.current,
                      [toolKey]: {
                        ...toolCallsRef.current[toolKey],
                        status: "running",
                      },
                    };
                    setToolCalls({ ...toolCallsRef.current });
                  },
                  onToolResult: (
                    index: number,
                    toolName: string,
                    input: string,
                    output: any,
                    isError: boolean,
                    durationMs?: number,
                  ) => {
                    markStreamActivity();
                    const toolKey =
                      liveToolKeyByIndexRef.current[index] ??
                      `resume:${index}:${Date.now()}`;
                    liveToolKeyByIndexRef.current[index] = toolKey;
                    const existing = toolCallsRef.current[toolKey];
                    const parsed = parseToolName(
                      toolName || existing?.name || "unknown",
                    );
                    toolCallsRef.current = {
                      ...toolCallsRef.current,
                      [toolKey]: {
                        ...(existing ?? {
                          index,
                          source: parsed.source,
                          args: "",
                          result: "",
                          isError: false,
                          status: "pending" as const,
                        }),
                        name: toolName || parsed.tool || "unknown",
                        args: input || existing?.args || "",
                        result:
                          typeof output === "string"
                            ? output
                            : JSON.stringify(output),
                        isError,
                        status: isError ? "error" : "success",
                        durationMs,
                      },
                    };
                    setToolCalls({ ...toolCallsRef.current });
                  },
                  onToolCall: (tool: any) => {
                    markStreamActivity();
                    const idx =
                      typeof tool.index === "number" &&
                      Number.isFinite(tool.index)
                        ? tool.index
                        : Object.keys(toolCallsRef.current).length;
                    const name = tool.tool_name ?? tool.name ?? "unknown";
                    const source = tool.source ?? parseToolName(name).source;
                    const key =
                      liveToolKeyByIndexRef.current[idx] ??
                      `resume:summary:${idx}:${Date.now()}`;
                    liveToolKeyByIndexRef.current[idx] = key;
                    toolCallsRef.current = {
                      ...toolCallsRef.current,
                      [key]: {
                        index: idx,
                        name,
                        source,
                        args:
                          typeof tool.input === "string"
                            ? tool.input
                            : JSON.stringify(tool.input ?? {}),
                        result:
                          typeof tool.output === "string"
                            ? tool.output
                            : JSON.stringify(tool.output ?? ""),
                        isError: tool.is_error ?? false,
                        status: (tool.is_error ?? false) ? "error" : "success",
                        durationMs: tool.duration_ms,
                      },
                    };
                    setToolCalls({ ...toolCallsRef.current });
                  },
                  onUsage: (u: any) => {
                    onUsage?.({
                      inputTokens: u.input_tokens ?? 0,
                      outputTokens: u.output_tokens ?? 0,
                      estimatedCostUsd: u.estimated_cost_usd,
                    });
                  },
                  onSuperAssistantAnswer: (payload) => {
                    markStreamActivity();
                    if (payload.kind !== "deepAnalysis") return;
                    const { link, taskId, status } = payload.answer;
                    if (!taskId) return;
                    resetPmResearchState();
                    superAssistantAsyncTaskStartedRef.current = true;
                    setPmBackgroundTaskId(taskId);
                    setPmBackgroundTaskStatus(status || "queued");
                    setPmPanelTaskId(taskId);
                    setPmPanelTaskStatus(status || "queued");
                    setPmPanelOpen((open) =>
                      superAssistantEndpoint ? open : true,
                    );
                    pmBackgroundTaskAbortRef.current?.();
                    pmBackgroundTaskAbortRef.current = null;

                    if (link === "pmResearchTask") {
                      pmBackgroundTaskAbortRef.current = streamPmResearchTask(
                        taskId,
                        {
                          onEvent: (event) => applyPmTaskEvent(event),
                          onAnswerDelta: (event) =>
                            appendPmAnswerDelta(event.delta),
                          onImageContextWarning: handlePmImageContextWarning,
                          onDone: (event) => {
                            applyPmTaskEvent(event);
                            flushVisibleStreamingText();
                            const doneStatus =
                              derivePmBackgroundTaskStatus(event);
                            setPmBackgroundTaskStatus(doneStatus);
                            setPmPanelTaskStatus(doneStatus);
                            pmBackgroundTaskAbortRef.current = null;
                            superAssistantAsyncTaskStartedRef.current = false;
                            setIsStreaming(false);
                            onStreamingChange?.(false);
                            appendPmTerminalMessageIfNeeded(event);
                          },
                          onError: (err) => {
                            pmBackgroundTaskAbortRef.current = null;
                            superAssistantAsyncTaskStartedRef.current = false;
                            setPmBackgroundTaskStatus("failed");
                            setPmPanelTaskStatus("failed");
                            setIsStreaming(false);
                            onStreamingChange?.(false);
                            message.error(`${t("chat.streamError")}: ${err}`);
                          },
                        },
                      );
                      return;
                    }
                    if (link === "chatAdversarialRun") {
                      pmBackgroundTaskAbortRef.current =
                        streamSuperAssistantAdversarialRun(taskId);
                      return;
                    }
                    if (link === "dataAttributionTask") {
                      pmBackgroundTaskAbortRef.current =
                        streamNl2sqlAttributionTask(taskId, {
                          onEvent: (event) => applyAttributionTaskEvent(event),
                          onDone: (event) => {
                            applyAttributionTaskEvent(event);
                            pmBackgroundTaskAbortRef.current = null;
                            superAssistantAsyncTaskStartedRef.current = false;
                            const doneStatus = normalizeAttributionTaskStatus(
                              event.status,
                            );
                            setPmBackgroundTaskStatus(doneStatus);
                            setPmPanelTaskStatus(doneStatus);
                            setIsStreaming(false);
                            onStreamingChange?.(false);
                            appendAttributionTerminalMessageIfNeeded(event);
                          },
                          onError: (err) => {
                            pmBackgroundTaskAbortRef.current = null;
                            superAssistantAsyncTaskStartedRef.current = false;
                            setPmBackgroundTaskStatus("failed");
                            setPmPanelTaskStatus("failed");
                            setIsStreaming(false);
                            onStreamingChange?.(false);
                            message.error(`${t("chat.streamError")}: ${err}`);
                          },
                        });
                    }
                  },
                  onStreamEnd: (
                    _iterations: number,
                    _usage?: AgentUsage,
                    fullText?: string,
                    finalThinking?: string,
                    meta?: {
                      streamMode?: string;
                      telemetry?: { cancelled?: boolean };
                    },
                  ) => {
                    markStreamActivity();
                    superAssistantTurnIdRef.current = null;
                    if (
                      meta?.telemetry?.cancelled ||
                      meta?.streamMode === "cancelled"
                    ) {
                      resetStreamingText();
                      setToolCalls({});
                      toolCallsRef.current = {};
                      liveToolIndicesRef.current = new Set();
                      liveToolKeyByIndexRef.current = {};
                      setIsStreaming(false);
                      onStreamingChange?.(false);
                      resetThinkingState();
                      return;
                    }
                    if (thinkingStartedAtRef.current != null) {
                      thinkingDurationRef.current =
                        Date.now() - thinkingStartedAtRef.current;
                      thinkingStartedAtRef.current = null;
                    }
                    const streamedAssistantText = (
                      fullText ||
                      streamingTextRef.current ||
                      ""
                    ).trim();
                    if (
                      superAssistantAsyncTaskStartedRef.current &&
                      !streamedAssistantText
                    ) {
                      streamCommittedRef.current = true;
                      resetStreamingText();
                      setToolCalls({});
                      toolCallsRef.current = {};
                      liveToolIndicesRef.current = new Set();
                      liveToolKeyByIndexRef.current = {};
                      setIsStreaming(true);
                      onStreamingChange?.(true);
                      resetThinkingState();
                      return;
                    }
                    const assistantText =
                      fullText ||
                      streamingTextRef.current ||
                      t("chat.noResponse");
                    if (assistantText !== streamingTextRef.current) {
                      streamingTextRef.current = assistantText;
                      setStreamingText(assistantText);
                      scheduleTypewriterDrain();
                    }
                    const completedToolCalls = Object.values(
                      toolCallsRef.current,
                    ).map((tc) =>
                      tc.status === "pending" || tc.status === "running"
                        ? { ...tc, status: "success" as const }
                        : tc,
                    );
                    const assistantMsg: DisplayMessage = {
                      id: `asst-resume-${Date.now()}`,
                      role: "assistant",
                      content: assistantText,
                      timestamp: Date.now(),
                      modelName:
                        activeResponseModelNameRef.current || undefined,
                      ...activeAdversarialMetaRef.current,
                      toolCalls:
                        completedToolCalls.length > 0
                          ? completedToolCalls
                          : undefined,
                      thinking:
                        finalThinking || thinkingTextRef.current || null,
                      thinkingDurationMs:
                        finalThinking || thinkingTextRef.current
                          ? thinkingDurationRef.current
                          : undefined,
                    };
                    const commit = () => {
                      streamCommittedRef.current = true;
                      setDisplayMessages((prev) => [...prev, assistantMsg]);
                      resetStreamingText();
                      setToolCalls({});
                      toolCallsRef.current = {};
                      liveToolIndicesRef.current = new Set();
                      liveToolKeyByIndexRef.current = {};
                      setIsStreaming(false);
                      onStreamingChange?.(false);
                      resetThinkingState();
                      onStreamFinished?.(
                        assistantMsg,
                        completedToolCalls,
                        assistantMsg.thinking ?? "",
                      );
                      void refreshSessionMemorySources(
                        sessionId,
                        turnStartedAtMs,
                      );
                    };
                    flushVisibleStreamingText();
                    commit();
                  },
                  onError: (error: string) => {
                    superAssistantAsyncTaskStartedRef.current = false;
                    message.error(`${t("chat.streamError")}: ${error}`);
                    resetStreamingText();
                    setToolCalls({});
                    toolCallsRef.current = {};
                    liveToolIndicesRef.current = new Set();
                    liveToolKeyByIndexRef.current = {};
                    setIsStreaming(false);
                    onStreamingChange?.(false);
                    resetThinkingState();
                  },
                },
              );
            })
            .catch(() => {
              // Best-effort resume; normal history remains usable.
            });
        }
      } catch {
        if (latestRequestRef.current !== loadToken) return;
        message.error(t("chat.loadFailed"));
        setDisplayMessages([]);
      }
    },
    [
      applySuperAssistantPmStage,
      buildPmStageNarrative,
      clearPmPromptQueue,
      ensurePmInlineSegment,
      sessionSource,
      activeSessionId,
      stopActiveTurn,
      t,
      resetPmResearchState,
      resetStreamingText,
      resetThinkingState,
      flushVisibleStreamingText,
      focusInputAndScrollToBottom,
      markStreamActivity,
      streamSuperAssistantAdversarialRun,
      superAssistantEndpoint,
      onStreamingChange,
      onUsage,
      onStreamFinished,
      refreshSessionMemorySources,
      rememberAdversarialMeta,
      rememberResponseModel,
      scheduleTypewriterDrain,
      scrollToBottom,
    ],
  );

  useEffect(() => {
    loadSessionMessagesRef.current = loadSessionMessages;
    return () => {
      loadSessionMessagesRef.current = null;
    };
  }, [loadSessionMessages]);

  useEffect(() => {
    if (!superAssistantEndpoint || !isStreaming || !activeSessionId) return;

    const recoverIfBackendFinished = async () => {
      if (streamRecoveryInFlightRef.current) return;
      const idleMs = Date.now() - lastStreamActivityAtRef.current;
      if (idleMs < STREAM_STALL_RECOVERY_IDLE_MS) return;

      streamRecoveryInFlightRef.current = true;
      try {
        const activeTurn =
          await agentApi.getSuperAssistantActiveTurn(activeSessionId);
        const status = (activeTurn.status ?? "").toLowerCase();
        const terminal =
          !activeTurn.active ||
          status === "completed" ||
          status === "complete" ||
          status === "done" ||
          status === "failed" ||
          status === "cancelled" ||
          status === "canceled";
        if (!terminal) {
          lastStreamActivityAtRef.current = Date.now();
          return;
        }

        abortRef.current?.();
        abortRef.current = null;
        await loadSessionMessages(activeSessionId);
      } catch {
        lastStreamActivityAtRef.current = Date.now();
      } finally {
        streamRecoveryInFlightRef.current = false;
      }
    };

    const timer = window.setInterval(() => {
      void recoverIfBackendFinished();
    }, STREAM_STALL_RECOVERY_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [
    activeSessionId,
    isStreaming,
    loadSessionMessages,
    superAssistantEndpoint,
  ]);

  const loadOlderSessionMessages = useCallback(async () => {
    if (!activeSessionId) return;
    if (!historyHasMore) return;
    if (historyBeforeTurnCursor == null) return;
    if (historyLoadingMore || historyLoadingMoreRef.current) return;
    const capturedToken = latestRequestRef.current;
    const listEl = messageListRef.current;
    const previousScrollHeight = listEl?.scrollHeight ?? 0;
    const previousScrollTop = listEl?.scrollTop ?? 0;
    historyLoadingMoreRef.current = true;
    setHistoryLoadingMore(true);
    try {
      const res = await agentApi.getSessionHistory(activeSessionId, {
        before_turn_cursor: historyBeforeTurnCursor,
        limit_turns: HISTORY_PAGE_LIMIT_TURNS,
        max_bytes: HISTORY_PAGE_MAX_BYTES,
      });
      const memoryCitationsResp = await agentApi
        .listSessionMemoryCitations(activeSessionId)
        .catch(() => ({
          sessionId: activeSessionId,
          items: [] as AgentMemoryCitation[],
        }));
      if (capturedToken && latestRequestRef.current !== capturedToken) return;
      const older = attachSuperAssistantTurnMetadata(
        mapHistoryMessages(res.messages, {
          source: runtimeSessionSource,
          idPrefix: `hist-${activeSessionId}-${res.page?.before_turn_cursor ?? historyBeforeTurnCursor}`,
        }),
        res.super_assistant_turns,
      );
      const olderWithMemory = attachMemoryCitationsToMessages(
        older,
        memoryCitationsResp.items ?? [],
      );
      void registerHistoricalImagesForSession(
        olderWithMemory,
        activeSessionId,
      ).then((records) => {
        if (capturedToken && latestRequestRef.current !== capturedToken) return;
        if (Object.keys(records).length > 0) {
          setChatFileRecords((prev) => ({ ...prev, ...records }));
        }
      });
      flushSync(() => {
        setDisplayMessages((prev) => mergeHistoryPages(olderWithMemory, prev));
      });
      if (listEl) {
        const heightDelta = Math.max(
          0,
          listEl.scrollHeight - previousScrollHeight,
        );
        listEl.scrollTop = previousScrollTop + heightDelta;
      }
      const nextBefore = res.page?.next_before_turn_cursor;
      setHistoryBeforeTurnCursor(
        typeof nextBefore === "number" ? nextBefore : null,
      );
      setHistoryHasMore(
        res.page?.has_more === true && typeof nextBefore === "number",
      );
    } catch {
      message.error(t("chat.loadFailed"));
    } finally {
      historyLoadingMoreRef.current = false;
      setHistoryLoadingMore(false);
    }
  }, [
    activeSessionId,
    historyBeforeTurnCursor,
    historyHasMore,
    historyLoadingMore,
    sessionSource,
    t,
  ]);

  const maybeLoadOlderFromScroll = useCallback(
    (el: HTMLDivElement) => {
      if (
        !activeSessionId ||
        !historyHasMore ||
        historyLoadingMore ||
        historyLoadingMoreRef.current
      ) {
        return;
      }
      if (historyBeforeTurnCursor == null) return;
      if (el.scrollTop <= HISTORY_AUTO_LOAD_TOP_THRESHOLD_PX) {
        void loadOlderSessionMessages();
      }
    },
    [
      activeSessionId,
      historyBeforeTurnCursor,
      historyHasMore,
      historyLoadingMore,
      loadOlderSessionMessages,
    ],
  );

  useEffect(() => {
    const el = messageListRef.current;
    if (!el || !activeSessionId || !historyHasMore || historyLoadingMore) {
      return;
    }
    if (displayMessages.length === 0) return;
    if (historyAutoFillRef.current) return;
    const contentFitsViewport = el.scrollHeight <= el.clientHeight + 8;
    if (!contentFitsViewport) return;
    historyAutoFillRef.current = true;
    void loadOlderSessionMessages().finally(() => {
      historyAutoFillRef.current = false;
    });
  }, [
    activeSessionId,
    displayMessages.length,
    historyHasMore,
    historyLoadingMore,
    loadOlderSessionMessages,
  ]);

  // ── Session CRUD ───────────────────────────────────────────────────────────────────────────
  const runtimeScenario = superAssistantEndpoint
    ? "chat"
    : sessionSource === "pm"
      ? "pm"
      : sessionSource === "chat"
        ? "chat"
        : "rd";

  const createRuntimeSession = useCallback(
    async () =>
      agentApi.createSession({
        source: runtimeSessionSource,
        scenario: runtimeScenario,
        model: requestedModel || undefined,
        locale: i18n.resolvedLanguage ?? i18n.language,
      }),
    [
      i18n.language,
      i18n.resolvedLanguage,
      requestedModel,
      runtimeScenario,
      runtimeSessionSource,
    ],
  );

  const handleNewSession = async () => {
    if (abortRef.current) {
      if (superAssistantEndpoint) {
        abortRef.current();
        abortRef.current = null;
      } else {
        stopActiveTurn(activeSessionId);
      }
    }
    setIsStreaming(false);
    resetStreamingText();
    setToolCalls({});
    toolCallsRef.current = {};
    liveToolIndicesRef.current = new Set();
    liveToolKeyByIndexRef.current = {};
    resetThinkingState();
    setHistoryHasMore(false);
    setHistoryBeforeTurnCursor(null);
    setHistoryLoadingMore(false);
    superAssistantTurnIdRef.current = null;
    resetPmResearchState();
    clearPmPromptQueue();
    try {
      const session = await createRuntimeSession();
      setActiveSessionId(session.session.session_id);
      setActiveMcpServers(sessionMetadataNames(session.session.mcp_servers));
      setActiveSkills(sessionMetadataNames(session.session.skills));
      rememberResponseModel(session.session.model);
      setDisplayMessages([]);
      setHistoryHasMore(false);
      setHistoryBeforeTurnCursor(null);
      setHistoryLoadingMore(false);
      focusInputAndScrollToBottom();
      onSessionCreated?.(session.session.session_id);
      await qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
    } catch (err) {
      message.error(
        `${t("chat.createSessionFailed")}: ${(err as Error).message}`,
      );
    }
  };

  const handleDeleteSession = async (sessionId: string) => {
    const wasActive = activeSessionId === sessionId;
    if (wasActive) {
      setActiveSessionId(null);
      setDisplayMessages([]);
      setHistoryHasMore(false);
      setHistoryBeforeTurnCursor(null);
      setHistoryLoadingMore(false);
      resetPmResearchState();
      clearPmPromptQueue();
    }
    try {
      await agentApi.deleteSession(sessionId);
      await qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
      if (wasActive) {
        const updated = qc.getQueryData<any>(
          queryKeys.agentSessions.list(runtimeSessionSource),
        );
        const remaining = (updated?.sessions ?? []).filter(
          (s: any) => s.session_id !== sessionId,
        );
        if (remaining.length > 0) {
          const next = remaining[0].session_id;
          setActiveSessionId(next);
          loadSessionMessages(next);
        }
      }
      message.success(t("chat.deleteSuccess"));
    } catch {
      if (wasActive) setActiveSessionId(sessionId);
      await qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
      message.error(t("chat.deleteFailed"));
    }
  };

  const handleRenameSession = async (sessionId: string, name: string) => {
    try {
      await agentApi.renameSession(sessionId, name);
      await qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
      message.success(t("chat.renameSuccess"));
    } catch {
      message.error(t("chat.renameFailed"));
    }
  };

  const handleTogglePin = async (sessionId: string) => {
    try {
      const res = await agentApi.togglePinSession(sessionId);
      await qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
      message.success(
        res.is_pinned ? t("chat.pinSuccess") : t("chat.unpinSuccess"),
      );
    } catch {
      message.error(t("chat.pinFailed"));
    }
  };

  const handleExportMarkdown = useCallback(() => {
    if (displayMessages.length === 0) {
      message.info(t("chat.exportEmpty", "当前会话暂无可导出的内容"));
      return;
    }
    const markdown = buildSessionMarkdown(
      activeSessionId,
      sessionSource,
      displayMessages,
      sessionSource === "pm" ? pmQualitySnapshot : null,
    );
    try {
      const now = new Date();
      const stamp = now.toISOString().replace(/[:.]/g, "-");
      const filename = `aos-${sessionSource}-${(activeSessionId ?? "session").slice(0, 12)}-${stamp}.md`;
      const blob = new Blob([markdown], {
        type: "text/markdown;charset=utf-8",
      });
      const url = window.URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      document.body.appendChild(link);
      link.click();
      document.body.removeChild(link);
      window.URL.revokeObjectURL(url);
      message.success(t("chat.exportSuccess", "Markdown 已下载"));
    } catch {
      message.error(t("chat.exportFailed", "下载失败，请稍后重试"));
    }
  }, [activeSessionId, displayMessages, pmQualitySnapshot, sessionSource, t]);

  const handleExportJson = useCallback(() => {
    if (displayMessages.length === 0) {
      message.info(t("chat.exportEmpty", "当前会话暂无可导出的内容"));
      return;
    }
    try {
      const now = new Date();
      const stamp = now.toISOString().replace(/[:.]/g, "-");
      const filename = `aos-${sessionSource}-${(activeSessionId ?? "session").slice(0, 12)}-${stamp}.json`;
      const payload = {
        sessionId: activeSessionId,
        source: sessionSource,
        exportedAt: now.toISOString(),
        messages: displayMessages.map((msg) => ({
          role: msg.role,
          content: msg.content,
          toolCalls: msg.toolCalls,
          evidenceSources: msg.evidenceSources,
          thinking: msg.thinking,
          timestamp: msg.timestamp,
          createdAt: msg.createdAt,
          replyTo: msg.replyTo,
        })),
        pmQuality: sessionSource === "pm" ? pmQualitySnapshot : null,
      };
      const blob = new Blob([JSON.stringify(payload, null, 2)], {
        type: "application/json;charset=utf-8",
      });
      const url = window.URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      document.body.appendChild(link);
      link.click();
      document.body.removeChild(link);
      window.URL.revokeObjectURL(url);
      message.success(t("chat.exportJsonSuccess", "JSON 已下载"));
    } catch {
      message.error(t("chat.exportFailed", "下载失败，请稍后重试"));
    }
  }, [activeSessionId, displayMessages, pmQualitySnapshot, sessionSource, t]);

  const handleToggleBookmark = async (sessionId: string) => {
    try {
      const res = await agentApi.toggleBookmarkSession(sessionId);
      await qc.invalidateQueries({
        queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
      });
      message.success(
        res.bookmarked
          ? t("chat.bookmarkSuccess")
          : t("chat.unbookmarkSuccess"),
      );
    } catch {
      message.error(t("chat.bookmarkFailed"));
    }
  };

  const appendLocalAssistantMessage = useCallback(
    (content: string) => {
      const assistantMsg: DisplayMessage = {
        id: `local-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        role: "assistant",
        content,
        timestamp: Date.now(),
        localCommand: true,
      };
      setDisplayMessages((prev) => [...prev, assistantMsg]);
      autoFollowScrollRef.current = true;
      setTimeout(() => scrollToBottom(true), 20);
    },
    [scrollToBottom],
  );

  const applyAttributionTaskEvent = useCallback(
    (event: AttributionTaskEvent) => {
      const taskId = event.task_id;
      const nextStatus = normalizeAttributionTaskStatus(event.status);
      setPmBackgroundTaskId(taskId);
      setPmBackgroundTaskStatus(nextStatus);
      setPmPanelTaskId(taskId);
      setPmPanelTaskStatus(nextStatus);
      setPmPanelOpen((open) => (superAssistantEndpoint ? open : true));
      setPmSuppressExecutionUi(false);

      const stageName = normalizeAttributionStage(event);
      const status = normalizeAttributionStageStatus(event);
      const at = Date.now();
      const detail = normalizeAttributionDetail(event);
      const previousStageState = pmStageStatesRef.current[stageName];
      const runningSince =
        status === "running"
          ? previousStageState?.status === "running"
            ? (previousStageState.runningSince ?? previousStageState.updatedAt)
            : at
          : previousStageState?.runningSince;
      const nextEntry: PmStageState = {
        stage: stageName,
        status,
        attempt: 1,
        detail,
        runningSince: status === "running" ? runningSince : undefined,
        updatedAt: at,
      };
      setPmStageStates((prev) => {
        const merged = { ...prev, [stageName]: nextEntry };
        pmStageStatesRef.current = merged;
        return merged;
      });
      setPmStageEvents((prev) => {
        const merged = [
          ...prev,
          {
            stage: stageName,
            status,
            attempt: 1,
            detail,
            at,
          },
        ].slice(-120);
        pmStageEventsRef.current = merged;
        return merged;
      });
      const summary =
        event.message || event.observation?.title || event.error || stageName;
      ensurePmInlineSegment(stageName, status, 1, summary, detail);
      if (isPmTaskTerminalStatus(nextStatus)) {
        pmActiveInlineSegmentIdRef.current = null;
        setIsStreaming(false);
        onStreamingChange?.(false);
      }
    },
    [ensurePmInlineSegment, onStreamingChange],
  );

  const appendAttributionTerminalMessageIfNeeded = useCallback(
    (event: AttributionTaskEvent) => {
      const finalText = formatAttributionTerminalMessage(event);
      if (!finalText.trim()) {
        streamCommittedRef.current = true;
        resetStreamingText();
        setIsStreaming(false);
        onStreamingChange?.(false);
        return;
      }
      if (streamingTextRef.current.trim()) {
        streamingTextRef.current = finalText;
        setStreamingText(finalText);
        flushVisibleStreamingText();
        resetStreamingText();
      }
      streamCommittedRef.current = true;
      const assistantMsg: DisplayMessage = {
        id: `attr-${event.task_id}-${Date.now()}`,
        role: "assistant",
        content: finalText,
        timestamp: Date.now(),
        modelName: activeResponseModelNameRef.current || undefined,
        attributionTaskId: event.task_id,
        superAssistantTurnId: superAssistantTurnIdRef.current || undefined,
      };
      setDisplayMessages((prev) => [...prev, assistantMsg]);
      setIsStreaming(false);
      onStreamingChange?.(false);
      setTimeout(scrollToBottom, 30);
    },
    [
      flushVisibleStreamingText,
      onStreamingChange,
      resetStreamingText,
      scrollToBottom,
    ],
  );

  const dispatchBuiltinSlashCommand = useCallback(
    async (rawInput: string): Promise<boolean> => {
      const trimmed = rawInput.trim();
      const parsed = parseWebSlashCommand(trimmed);
      if (!parsed) return false;
      const { name, args } = parsed;
      const registered = allSlashCommands.find(
        (cmd) => cmd.name.toLowerCase() === name,
      );
      if (registered?.source === "skill") return false;

      setInput("");
      setSlashOpen(false);
      setSlashFilter("");
      setSlashSelected(0);

      if (!registered && !WEB_BUILTIN_SLASH_COMMANDS.has(name)) {
        appendLocalAssistantMessage(
          t(
            "chat.slashCommandUnknown",
            "Unknown local command `/{{command}}`. Type `/commands` to inspect the commands available in this web session.",
            { command: name },
          ),
        );
        return true;
      }

      switch (name) {
        case "commands": {
          setSlashOpen(true);
          setSlashFilter("");
          setSlashSelected(0);
          window.setTimeout(() => composerRef.current?.focus(), 0);
          return true;
        }
        case "help": {
          const rows = allSlashCommands
            .filter((cmd) => cmd.source === "builtin")
            .map(
              (cmd) =>
                `- \`/${cmd.name}\`${cmd.hint ? ` ${cmd.hint.replace(/^\/\S+\s*/, "")}` : ""}: ${cmd.description}`,
            )
            .join("\n");
          appendLocalAssistantMessage(
            `${t("chat.slashHelpTitle", "Available built-in commands")}\n\n${rows}`,
          );
          return true;
        }
        case "compact": {
          const result = await handleManualCompact();
          if (result) {
            appendLocalAssistantMessage(
              [
                `## ${t("chat.compactResult", "Context compacted")}`,
                "",
                `- ${t("chat.compactRemovedMessages", "removed {{count}} messages", { count: result.removedMessageCount })}`,
                `- ${t("chat.compactSummaryTokens", "summary {{count}} tokens", { count: result.summaryTokens })}`,
                `- ${t("chat.compactRetainedTail", "tail {{count}} tokens", { count: result.retainedTailTokens })}`,
                `- ${t("chat.compactStrategy", "Strategy")}: ${result.strategy}`,
              ].join("\n"),
            );
          }
          return true;
        }
        case "clear":
        case "session": {
          if (
            name === "session" &&
            args &&
            !/^(new|clear|reset)$/i.test(args)
          ) {
            appendLocalAssistantMessage(
              t(
                "chat.slashCommandUnsupportedDetail",
                "This web chat currently supports `/session new` only. Use the session list for save/load/delete.",
              ),
            );
            return true;
          }
          await handleNewSession();
          message.success(
            t("chat.slashSessionNew", "Started a fresh session."),
          );
          return true;
        }
        case "export": {
          if (args.toLowerCase() === "json") {
            handleExportJson();
          } else {
            handleExportMarkdown();
          }
          return true;
        }
        case "memory": {
          setMemoryDrawerOpen(true);
          appendLocalAssistantMessage(
            t("chat.slashMemoryOpened", "Opened the Memory panel."),
          );
          return true;
        }
        case "status": {
          let contextLine = t(
            "chat.contextUnavailable",
            "Context status is unavailable.",
          );
          if (activeSessionId) {
            try {
              const status =
                await agentApi.getSessionContextStatus(activeSessionId);
              contextLine = t(
                "chat.contextUsage",
                "Estimated context: {{used}} / {{limit}} tokens",
                {
                  used: status.estimatedTokens.toLocaleString(),
                  limit: status.effectiveContextLimit.toLocaleString(),
                },
              );
            } catch {
              contextLine = t(
                "chat.contextUnavailable",
                "Context status is unavailable.",
              );
            }
          }
          appendLocalAssistantMessage(
            [
              `## ${t("chat.slashStatusTitle", "Session status")}`,
              "",
              `- ${t("chat.sessionPrefix", "Session: ")}${activeSessionId ?? t("chat.newConversation", "New conversation")}`,
              `- ${t("chat.model", "Model")}: ${commandModelOverride || activeResponseModelNameRef.current || selectedSession?.model || selectedModel || "-"}`,
              `- ${t("chat.search", "Search")}: ${searchMode}`,
              `- ${t("chat.memoryPanelTitle", "Memory")}: ${memoryPaused ? t("chat.memoryPaused", "Memory paused") : t("chat.memoryOn", "Memory on")}`,
              `- ${contextLine}`,
            ].join("\n"),
          );
          return true;
        }
        case "model": {
          const currentModel = resolveEffectiveModel(
            commandModelOverride,
            activeResponseModelNameRef.current,
            selectedSession?.model,
            selectedModel,
          );
          if (/^(auto|default|reset)$/i.test(args)) {
            setCommandModelOverride(null);
            if (!superAssistantEndpoint && activeSessionId) {
              await handleNewSession();
            }
            appendLocalAssistantMessage(
              t(
                "chat.slashModelReset",
                "Model override cleared. The next turn will use the session/default model: {{model}}.",
                { model: selectedSession?.model || selectedModel || "auto" },
              ),
            );
            return true;
          }
          if (args) {
            setCommandModelOverride(args);
            rememberResponseModel(args);
            if (!superAssistantEndpoint && activeSessionId) {
              await handleNewSession();
            }
            appendLocalAssistantMessage(
              t(
                "chat.slashModelChanged",
                "Model override for subsequent turns: {{model}}. The backend will reject it if no enabled API key provides this model.",
                { model: args },
              ),
            );
            return true;
          }
          appendLocalAssistantMessage(
            currentModel
              ? t("chat.slashModelCurrent", "Current model: {{model}}", {
                  model: currentModel,
                })
              : t(
                  "chat.slashModelUnset",
                  "No model override is selected. Use the model selector in the page header.",
                ),
          );
          return true;
        }
        case "permissions": {
          const permissionMode = selectedSession?.permissionMode || "default";
          appendLocalAssistantMessage(
            args
              ? t(
                  "chat.slashPermissionsImmutable",
                  "Current permission mode: {{mode}}. Web session permissions are enforced by the account and cannot be escalated from a chat command.",
                  { mode: permissionMode },
                )
              : t(
                  "chat.slashPermissionsCurrent",
                  "Current permission mode: {{mode}}",
                  {
                    mode: permissionMode,
                  },
                ),
          );
          return true;
        }
        case "mcp": {
          if (args.toLowerCase() === "help") {
            appendLocalAssistantMessage(
              [
                `## ${t("chat.slashMcpHelpTitle", "MCP commands")}`,
                "",
                `- \`/mcp\`: ${t("chat.slashMcpHelpCurrent", "show active MCP servers")}`,
                `- \`/mcp list\`: ${t("chat.slashMcpHelpList", "show active MCP servers")}`,
                `- \`/mcp help\`: ${t("chat.slashMcpHelpHelp", "show this help")}`,
                "",
                `### ${t("chat.slashMcpTitle", "Active MCP servers")}`,
                ...(effectiveMcpServers.length > 0
                  ? effectiveMcpServers.map((item) => `- ${item}`)
                  : [
                      t(
                        "chat.slashMcpEmpty",
                        "No MCP servers are active in this session.",
                      ),
                    ]),
              ].join("\n"),
            );
            return true;
          }
          appendLocalAssistantMessage(
            slashListMarkdown(
              t("chat.slashMcpTitle", "Active MCP servers"),
              effectiveMcpServers,
              t(
                "chat.slashMcpEmpty",
                "No MCP servers are active in this session.",
              ),
            ),
          );
          return true;
        }
        case "skills": {
          if (args.toLowerCase() === "help") {
            appendLocalAssistantMessage(
              [
                `## ${t("chat.slashSkillsHelpTitle", "Skills commands")}`,
                "",
                `- \`/skills\`: ${t("chat.slashSkillsHelpCurrent", "show enabled skills")}`,
                `- \`/skills list\`: ${t("chat.slashSkillsHelpList", "show enabled skills")}`,
                `- \`/skills help\`: ${t("chat.slashSkillsHelpHelp", "show this help")}`,
                `- \`/<skill-name> [args]\`: ${t("chat.slashSkillsHelpRun", "run an enabled skill")}`,
                "",
                `### ${t("chat.slashSkillsTitle", "Enabled skills")}`,
                ...(effectiveSkills.length > 0
                  ? effectiveSkills.map((item) => `- ${item}`)
                  : [
                      t(
                        "chat.slashSkillsEmpty",
                        "No skills are active in this session. Install or enable skills from the Skills menu.",
                      ),
                    ]),
              ].join("\n"),
            );
            return true;
          }
          appendLocalAssistantMessage(
            slashListMarkdown(
              t("chat.slashSkillsTitle", "Enabled skills"),
              effectiveSkills,
              t(
                "chat.slashSkillsEmpty",
                "No skills are active in this session. Install or enable skills from the Skills menu.",
              ),
            ),
          );
          return true;
        }
        case "cost": {
          appendLocalAssistantMessage(
            t(
              "chat.slashCostHint",
              "Token and cost usage is shown in the message usage bar and Governance/usage dashboards when provider usage is returned.",
            ),
          );
          return true;
        }
        default:
          appendLocalAssistantMessage(
            t(
              "chat.slashCommandUnknown",
              "Unknown local command `/{{command}}`. Type `/commands` to inspect available commands.",
              {
                command: name,
              },
            ),
          );
          return true;
      }
    },
    [
      activeSessionId,
      allSlashCommands,
      appendLocalAssistantMessage,
      effectiveMcpServers,
      effectiveSkills,
      handleExportJson,
      handleExportMarkdown,
      handleManualCompact,
      handleNewSession,
      memoryPaused,
      commandModelOverride,
      rememberResponseModel,
      searchMode,
      selectedModel,
      selectedSession?.model,
      selectedSession?.permissionMode,
      superAssistantEndpoint,
      t,
    ],
  );

  // ── Stream send ──────────────────────────────────────────────────────────────────────────────
  const handleSend = async (overrideMessage?: string) => {
    const override = (overrideMessage ?? "").trim();
    const rawInput = override.length > 0 ? override : draftInputRef.current;
    const superAssistantSlash = superAssistantEndpoint
      ? parseSuperAssistantSlashCommand(rawInput)
      : null;
    if (superAssistantSlash && superAssistantSlash.prompt.length === 0) {
      message.warning(
        t(
          "superAssistant.slashCommandNeedsPrompt",
          "请在指令后输入要处理的问题。",
        ),
      );
      return;
    }
    if (superAssistantSlash?.mode === "super_adversarial") {
      try {
        const apiKeys = await qc.fetchQuery({
          queryKey: queryKeys.apiKeys.list(),
          queryFn: () => apiKeysApi.list(),
          staleTime: 5_000,
        });
        if (countDistinctUsableChatModels(apiKeys.keys) < 2) {
          message.warning(t("chat.adversarialNeedModels"));
          return;
        }
      } catch {
        // The backend repeats this check authoritatively before creating a turn.
      }
    }
    const slashRequestOptions = superAssistantSlash
      ? superAssistantSlashRequestOptions(superAssistantSlash.mode)
      : {};
    const effectiveInput = superAssistantSlash?.prompt ?? rawInput;
    if (
      override.length === 0 &&
      /^\/[^\s]+(?:\s|$)/.test(effectiveInput.trim())
    ) {
      const handled = await dispatchBuiltinSlashCommand(effectiveInput);
      if (handled) return;
    }
    const effectiveAttachments = override.length > 0 ? [] : attachments;
    const usePmBackground = sessionSource === "pm" && !superAssistantEndpoint;
    const pmAttachmentMeta = usePmBackground
      ? collectPmTaskAttachments(effectiveAttachments)
      : {
          images: [] as PmTaskImageInput[],
          documents: [] as PmTaskDocumentInput[],
          hasUnsupportedImageSource: false,
          hasUnsupportedDocumentSource: false,
        };
    if (
      usePmBackground &&
      (pmAttachmentMeta.hasUnsupportedImageSource ||
        pmAttachmentMeta.hasUnsupportedDocumentSource)
    ) {
      message.warning(
        t(
          "operations.pmBackgroundImageSourceUnsupported",
          "后台研究仅支持已上传附件，请重新上传后再试。",
        ),
      );
      return;
    }
    if (
      usePmBackground &&
      pmAttachmentMeta.images.length > PM_MAX_IMAGE_ATTACHMENTS
    ) {
      message.warning(
        t(
          "operations.pmBackgroundImageLimit",
          "后台研究最多支持 5 张图片，请减少附件后重试。",
        ),
      );
      return;
    }
    const content = buildMessageContent(effectiveInput, effectiveAttachments);
    const visibleContent = superAssistantSlash
      ? buildMessageContent(rawInput, effectiveAttachments)
      : content;
    const isEmpty =
      typeof content === "string" ? !content.trim() : content.length === 0;
    if (isEmpty) return;

    if (isStreaming && superAssistantEndpoint) {
      message.warning({ content: t("chat.sessionBusy"), duration: 6 });
      return;
    }

    if (isStreaming && !usePmBackground) {
      stopActiveTurn(activeSessionId);
      resetStreamingText();
      setToolCalls({});
      toolCallsRef.current = {};
      liveToolKeyByIndexRef.current = {};
      setIsStreaming(false);
    }

    setInput("");
    if (usePmBackground) {
      setIsStreaming(false);
      onStreamingChange?.(false);
    } else {
      setIsStreaming(true);
      onStreamingChange?.(true);
    }
    streamCommittedRef.current = false;
    resetStreamingText();
    setToolCalls({});
    toolCallsRef.current = {};
    liveToolIndicesRef.current = new Set();
    liveToolKeyByIndexRef.current = {};
    // Pristine reasoning state before a new send. `thinkingLoadingRef`
    // is flipped on afterwards because we expect the first delta to
    // arrive shortly, but the duration timer stays `null` until
    // `thinking_start` actually fires.
    resetThinkingState();
    if (!usePmBackground) {
      resetPmResearchState();
    }
    thinkingLoadingRef.current = !usePmBackground;
    setThinkingLoading(!usePmBackground);

    setAttachments([]);
    setReplyingTo(null);

    let sessionId = activeSessionId;
    let effectiveTurnModel = resolveEffectiveModel(
      requestedModel,
      activeResponseModelNameRef.current,
      selectedSession?.model,
    );
    const turnStartedAtMs = Date.now();
    markStreamActivity();
    setStreamingMessageTimestamp(turnStartedAtMs);
    if (!sessionId) {
      try {
        const session = await createRuntimeSession();
        sessionId = session.session.session_id;
        setActiveSessionId(sessionId);
        setActiveMcpServers(sessionMetadataNames(session.session.mcp_servers));
        setActiveSkills(sessionMetadataNames(session.session.skills));
        effectiveTurnModel =
          session.session.model?.trim() || effectiveTurnModel;
        rememberResponseModel(effectiveTurnModel);
        onSessionCreated?.(sessionId);
        await qc.invalidateQueries({
          queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
        });
      } catch (err) {
        message.error(`${t("chat.streamError")}: ${(err as Error).message}`);
        setIsStreaming(false);
        onStreamingChange?.(false);
        return;
      }
    }
    rememberResponseModel(effectiveTurnModel);

    const textContent =
      typeof content === "string"
        ? content
        : usePmBackground
          ? content
              .filter((c) => c.type === "text")
              .map((c) => ("text" in c ? c.text : ""))
              .filter(Boolean)
              .join("\n")
          : content
              .filter((c) => c.type === "text")
              .map((c) => ("text" in c ? c.text : ""))
              .filter(Boolean)
              .join("\n");
    const streamImages = usePmBackground
      ? []
      : collectStreamImages(effectiveAttachments);
    const streamDocuments = usePmBackground
      ? []
      : collectStreamDocuments(effectiveAttachments);
    const evidenceDocuments = usePmBackground
      ? []
      : effectiveAttachments.filter(
          (att): att is DocumentBlock => att.type === "document",
        );
    const streamFileIds = effectiveAttachments
      .filter((att): att is DocumentBlock => att.type === "document")
      .map((att) => att.fileId)
      .filter((fileId): fileId is string => !!fileId);
    const attachedDocuments = effectiveAttachments.filter(
      (att): att is DocumentBlock => att.type === "document",
    );
    if (
      !textContent.trim() &&
      ((!usePmBackground &&
        streamImages.length === 0 &&
        streamDocuments.length === 0) ||
        (usePmBackground &&
          pmAttachmentMeta.images.length === 0 &&
          pmAttachmentMeta.documents.length === 0))
    ) {
      setIsStreaming(false);
      onStreamingChange?.(false);
      return;
    }

    const repliedPrefix = buildReplyPrefix(replyingTo, displayMessages);
    const imageOnlyPrompt =
      !usePmBackground && streamImages.length > 0 && !textContent.trim()
        ? t("chat.imageOnlyPrompt", "请分析我上传的图片，并回答关键内容。")
        : "";
    const documentOnlyPrompt =
      !usePmBackground &&
      streamDocuments.length > 0 &&
      !textContent.trim() &&
      !imageOnlyPrompt
        ? t("chat.documentOnlyPrompt", "请分析我上传的文档，并回答关键内容。")
        : "";
    const finalMessage =
      repliedPrefix + (textContent || imageOnlyPrompt || documentOnlyPrompt);
    if (attachedDocuments.length > 0 && sessionId) {
      try {
        const indexed = await registerUploadedDocumentsForSession(
          attachedDocuments,
          sessionId,
        );
        if (Object.keys(indexed).length > 0) {
          setChatFileRecords((prev) => ({ ...prev, ...indexed }));
        }
        const failed = Object.values(indexed).filter(
          (record) => record.status === "failed",
        );
        if (failed.length > 0) {
          message.warning(
            `${t("chat.fileIndexFailed", "File indexing failed")}: ${failed.map((record) => record.filename).join(", ")}`,
          );
        }
      } catch (err) {
        message.warning(
          `${t("chat.fileIndexFailed", "File indexing failed")}: ${(err as Error).message}`,
        );
      }
    }

    if (usePmBackground) {
      const userMessageId = `user-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const queuedPrompt: PmQueuedPromptDraft = {
        userMessageId,
        content,
        finalMessage,
        images: pmAttachmentMeta.images,
        documents: pmAttachmentMeta.documents,
        source: override.length > 0 ? "quick_fix" : "input",
        appendUserMessage: true,
        replyTo: replyingTo ?? undefined,
      };
      const currentPmBusy =
        pmQueueStartingRef.current ||
        pmBackgroundTaskAbortRef.current !== null ||
        pmBackgroundTaskStatus === "queued" ||
        pmBackgroundTaskStatus === "running" ||
        pmBackgroundTaskStatus === "cancelling";
      if (currentPmBusy) {
        enqueuePmPrompt(queuedPrompt);
      } else {
        const taskId = `pm-direct-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
        await startPmQueuedPrompt(
          {
            ...queuedPrompt,
            id: taskId,
            userMessageId,
            createdAt: Date.now(),
          },
          sessionId!,
        );
      }
      return;
    }

    const userMsg: DisplayMessage = {
      id: `user-${Date.now()}`,
      role: "user",
      content: visibleContent,
      timestamp: turnStartedAtMs,
      replyTo: replyingTo ?? undefined,
    };
    setDisplayMessages((prev) => [...prev, userMsg]);
    onBeforeStream?.(userMsg);
    autoFollowScrollRef.current = true;
    setTimeout(() => scrollToBottom(true), 50);

    const turnOptions: ChatTurnOptions | undefined =
      sessionSource === "chat"
        ? {
            searchMode: webSearchAvailable ? searchMode : "off",
            searchEnabled: webSearchAvailable && searchMode === "on",
            fileContext: {
              mode:
                streamDocuments.length > 0
                  ? "all_attached"
                  : streamFileIds.length > 0
                    ? "workspace"
                    : "none",
              fileIds: streamFileIds,
              strictGrounding: false,
            },
            memoryMode: "auto",
          }
        : undefined;

    superAssistantAsyncTaskStartedRef.current = false;
    if (superAssistantEndpoint) {
      superAssistantTurnIdRef.current = null;
    }
    const streamHandlers: AgentSessionStreamHandlers = {
      onApprovalRequired: (paused) => {
        if (activeSessionIdRef.current !== sessionId) return;
        markStreamActivity();
        setApprovalResolvingId(null);
        approvalPausedRef.current = paused;
        setApprovalPaused(paused);
        setIsStreaming(false);
        onStreamingChange?.(false);
      },
      onSuperAssistantTurnId: (turnId) => {
        superAssistantTurnIdRef.current = turnId;
      },
        onSessionActivated: (meta: any) => {
          markStreamActivity();
          if (meta.mcp_servers) setActiveMcpServers(meta.mcp_servers);
          if (meta.skills) setActiveSkills(meta.skills);
          rememberResponseModel(meta.model);
        },
        onConfigHotReload: (meta: any) => {
          markStreamActivity();
          if (meta.mcp_servers) setActiveMcpServers(meta.mcp_servers);
          if (meta.skills) setActiveSkills(meta.skills);
          rememberResponseModel(meta.model);
        },
        onThinkingStart: () => {
          markStreamActivity();
          syntheticThinkingHintRef.current = false;
          // Mark the start of the reasoning stream. The duration is
          // frozen at `thinking_end` or at the first `text_delta` that
          // closes the block — whichever comes first — so users see a
          // stable "X.Xs" label in the done-state bubble.
          thinkingStartedAtRef.current = Date.now();
          thinkingDurationRef.current = undefined;
          setThinkingDurationMs(undefined);
          setTimeout(scrollToBottom, 10);
        },
        onThinkingDelta: (text: string) => {
          markStreamActivity();
          if (syntheticThinkingHintRef.current) {
            thinkingTextRef.current = "";
            setThinkingText("");
            syntheticThinkingHintRef.current = false;
          }
          thinkingLoadingRef.current = false;
          setThinkingLoading(false);
          thinkingTextRef.current += text;
          setThinkingText(thinkingTextRef.current);
          setTimeout(scrollToBottom, 10);
        },
        onThinkingEnd: () => {
          markStreamActivity();
          // The reasoning stream closed cleanly. Freeze the duration
          // (if we captured a start timestamp) and flip loading off so
          // the bubble switches to its "已深度思考 · Xs" done state even
          // while the rest of the turn is still streaming text.
          if (thinkingStartedAtRef.current != null) {
            const dur = Date.now() - thinkingStartedAtRef.current;
            thinkingDurationRef.current = dur;
            setThinkingDurationMs(dur);
            thinkingStartedAtRef.current = null;
          }
          thinkingLoadingRef.current = false;
          setThinkingLoading(false);
        },
        onTextBlockStart: () => {
          markStreamActivity();
          // Many OpenAI-compat providers never emit an explicit
          // `thinking_end` — the first `text_delta` is the only signal.
          // Close the reasoning block here too so the "思考中…" label
          // doesn't linger after the first character of the answer.
          if (thinkingStartedAtRef.current != null) {
            const dur = Date.now() - thinkingStartedAtRef.current;
            thinkingDurationRef.current = dur;
            setThinkingDurationMs(dur);
            thinkingStartedAtRef.current = null;
          }
          thinkingLoadingRef.current = false;
          setThinkingLoading(false);
          setTimeout(scrollToBottom, 10);
        },
        onText: (text: string) => {
          markStreamActivity();
          const next = streamingTextRef.current + text;
          streamingTextRef.current = next;
          setStreamingText(next);
          scheduleTypewriterDrain();
          appendPmInlineExcerpt(text);
        },
        onTextBlockEnd: () => {},
        onToolUseStart: (index: number, id: string, name: string) => {
          markStreamActivity();
          if (!thinkingTextRef.current && !streamingTextRef.current) {
            const hint = t("chat.pmToolPlanningHint", "正在查找相关信息...");
            syntheticThinkingHintRef.current = true;
            thinkingLoadingRef.current = false;
            setThinkingLoading(false);
            thinkingTextRef.current = hint;
            setThinkingText(hint);
          }
          const parsed = parseToolName(name);
          const retrieveDetailRaw = pmStageStatesRef.current.retrieve?.detail;
          const retrieveDetail =
            retrieveDetailRaw &&
            typeof retrieveDetailRaw === "object" &&
            !Array.isArray(retrieveDetailRaw)
              ? (retrieveDetailRaw as Record<string, unknown>)
              : undefined;
          const warmQuery = pickPmDetailString(
            retrieveDetail,
            "selectedVariant",
          );
          const warmRoute = pickPmDetailString(retrieveDetail, "selectedRoute");
          const warmChannel = pickPmDetailString(
            retrieveDetail,
            "selectedRouteChannel",
          );
          const warmArgsObj: Record<string, string> = {};
          if (warmQuery) warmArgsObj.query = warmQuery;
          if (warmRoute) warmArgsObj.route = warmRoute;
          if (warmChannel) warmArgsObj.channel = warmChannel;
          const warmArgs =
            Object.keys(warmArgsObj).length > 0
              ? JSON.stringify(warmArgsObj)
              : "";
          const entry: ToolCallInfo = {
            index,
            name: parsed.tool,
            source: parsed.source,
            mcpServer: parsed.source === "mcp" ? parsed.sourceName : undefined,
            skillName:
              parsed.source === "skill" ? parsed.sourceName : undefined,
            args: "",
            result: "",
            isError: false,
            status: "pending",
          };
          const segmentId =
            pmActiveInlineSegmentIdRef.current ??
            pmInlineSegmentsRef.current[pmInlineSegmentsRef.current.length - 1]
              ?.id ??
            "retrieve#1";
          const toolKey = id
            ? `${segmentId}:${id}`
            : `${segmentId}:${index}:${Date.now()}`;
          liveToolKeyByIndexRef.current[index] = toolKey;
          toolCallsRef.current = { ...toolCallsRef.current, [toolKey]: entry };
          liveToolIndicesRef.current.add(toolKey);
          setToolCalls({ ...toolCallsRef.current });
          const readable = describePmInlineAction(
            name,
            warmArgs,
            "",
            "pending",
          );
          upsertPmInlineAction(toolKey, index, {
            name,
            source: parsed.source,
            status: "pending",
            detail: readable,
          });
          setTimeout(scrollToBottom, 50);
        },
        onToolInputDelta: (index: number, partialJson: string) => {
          markStreamActivity();
          const toolKey =
            liveToolKeyByIndexRef.current[index] ??
            Object.entries(toolCallsRef.current).reduce<string | undefined>(
              (matched, [key, value]) =>
                value.index === index ? key : matched,
              undefined,
            );
          if (!toolKey) return;
          const existing = toolCallsRef.current[toolKey];
          if (!existing) return;
          const merged: ToolCallInfo = {
            ...existing,
            args: mergeToolInput(existing.args, partialJson),
          };
          toolCallsRef.current = { ...toolCallsRef.current, [toolKey]: merged };
          setToolCalls({ ...toolCallsRef.current });
          upsertPmInlineAction(toolKey, index, {
            detail: describePmInlineAction(
              merged.name,
              merged.args,
              merged.result,
              merged.status === "error" ? "error" : "running",
            ),
          });
        },
        onToolUseEnd: (index: number) => {
          markStreamActivity();
          const toolKey =
            liveToolKeyByIndexRef.current[index] ??
            Object.entries(toolCallsRef.current).reduce<string | undefined>(
              (matched, [key, value]) =>
                value.index === index ? key : matched,
              undefined,
            );
          if (!toolKey) return;
          const existing = toolCallsRef.current[toolKey];
          if (!existing) return;
          const runningEntry: ToolCallInfo = {
            ...existing,
            status: "running" as const,
          };
          const retrieveDetailRaw = pmStageStatesRef.current.retrieve?.detail;
          const retrieveDetail =
            retrieveDetailRaw &&
            typeof retrieveDetailRaw === "object" &&
            !Array.isArray(retrieveDetailRaw)
              ? (retrieveDetailRaw as Record<string, unknown>)
              : undefined;
          const warmQuery = pickPmDetailString(
            retrieveDetail,
            "selectedVariant",
          );
          const warmRoute = pickPmDetailString(retrieveDetail, "selectedRoute");
          const warmChannel = pickPmDetailString(
            retrieveDetail,
            "selectedRouteChannel",
          );
          const warmArgsObj: Record<string, string> = {};
          if (warmQuery) warmArgsObj.query = warmQuery;
          if (warmRoute) warmArgsObj.route = warmRoute;
          if (warmChannel) warmArgsObj.channel = warmChannel;
          const warmArgs =
            Object.keys(warmArgsObj).length > 0
              ? JSON.stringify(warmArgsObj)
              : "";
          toolCallsRef.current = {
            ...toolCallsRef.current,
            [toolKey]: runningEntry,
          };
          setToolCalls({ ...toolCallsRef.current });
          upsertPmInlineAction(toolKey, index, {
            status: "running",
            detail: describePmInlineAction(
              runningEntry.name,
              runningEntry.args || warmArgs,
              "",
              "running",
            ),
          });
          setTimeout(scrollToBottom, 50);
        },
        onToolResult: (
          index: number,
          toolName: string,
          input: string,
          output: any,
          isError: boolean,
          durationMs?: number,
        ) => {
          markStreamActivity();
          const toolKey =
            liveToolKeyByIndexRef.current[index] ??
            Object.entries(toolCallsRef.current).reduce<string | undefined>(
              (matched, [key, value]) =>
                value.index === index ? key : matched,
              undefined,
            ) ??
            `${pmActiveInlineSegmentIdRef.current ?? "retrieve#1"}:${index}`;
          liveToolKeyByIndexRef.current[index] = toolKey;
          const existing = toolCallsRef.current[toolKey];
          const parsedFallback = parseToolName(toolName || "unknown");
          const mergedEntry: ToolCallInfo = {
            ...(existing ?? {
              index,
              name: parsedFallback.tool || toolName || "unknown",
              source: parsedFallback.source,
              mcpServer:
                parsedFallback.source === "mcp"
                  ? parsedFallback.sourceName || undefined
                  : undefined,
              skillName:
                parsedFallback.source === "skill"
                  ? parsedFallback.sourceName || undefined
                  : undefined,
              args: "",
              result: "",
              isError: false,
              status: "pending" as const,
            }),
            name:
              toolName || existing?.name || parsedFallback.tool || "unknown",
            args: input || existing?.args || "",
            result:
              typeof output === "string" ? output : JSON.stringify(output),
            isError,
            status: isError ? "error" : "success",
            durationMs,
          };
          toolCallsRef.current = {
            ...toolCallsRef.current,
            [toolKey]: mergedEntry,
          };
          setToolCalls({ ...toolCallsRef.current });
          upsertPmInlineAction(toolKey, index, {
            name: toolName,
            status: isError ? "error" : "success",
            durationMs,
            detail: describePmInlineAction(
              mergedEntry.name || toolName || "tool",
              mergedEntry.args || input,
              mergedEntry.result ||
                (typeof output === "string" ? output : JSON.stringify(output)),
              isError ? "error" : "success",
            ),
          });
          setTimeout(scrollToBottom, 50);
        },
        onToolCall: (tool: any) => {
          markStreamActivity();
          const name = tool.tool_name ?? "unknown";
          const source = tool.source as "mcp" | "builtin" | "skill";
          const idx =
            typeof tool.index === "number" && Number.isFinite(tool.index)
              ? tool.index
              : undefined;
          const key =
            idx != null
              ? (liveToolKeyByIndexRef.current[idx] ??
                `${pmActiveInlineSegmentIdRef.current ?? "retrieve#summary"}:${idx}:${Date.now()}`)
              : `${source}:${name}:${Date.now()}`;
          const existing = toolCallsRef.current[key];
          const entry: ToolCallInfo = {
            index: idx ?? existing?.index ?? 0,
            name: name || existing?.name || "unknown",
            source: source || existing?.source || "builtin",
            mcpServer:
              source === "mcp"
                ? tool.source_name || existing?.mcpServer || undefined
                : undefined,
            skillName:
              source === "skill"
                ? tool.source_name || existing?.skillName || undefined
                : undefined,
            args:
              typeof tool.input === "string"
                ? tool.input
                : JSON.stringify(tool.input ?? existing?.args ?? {}),
            result:
              typeof tool.output === "string"
                ? tool.output
                : JSON.stringify(tool.output ?? existing?.result ?? ""),
            isError: tool.is_error ?? false,
            status: (tool.is_error ?? false) ? "error" : "success",
            durationMs: tool.duration_ms,
          };
          toolCallsRef.current = { ...toolCallsRef.current, [key]: entry };
          setToolCalls({ ...toolCallsRef.current });
          if (idx != null) {
            liveToolKeyByIndexRef.current[idx] = key;
          }
          upsertPmInlineAction(key, entry.index, {
            name: entry.name,
            source: entry.source,
            status: entry.status === "error" ? "error" : "success",
            durationMs: entry.durationMs,
            detail: describePmInlineAction(
              entry.name,
              entry.args,
              entry.result,
              entry.status === "error" ? "error" : "success",
            ),
          });
          setTimeout(scrollToBottom, 50);
        },
        onUsage: (u: any) => {
          const uData = {
            inputTokens: u.input_tokens ?? 0,
            outputTokens: u.output_tokens ?? 0,
            estimatedCostUsd: u.estimated_cost_usd,
          };
          onUsage?.(uData);
        },
        onPmStage: applySuperAssistantPmStage,
        onPmQuality: (quality) => {
          markStreamActivity();
          setPmQualitySnapshot(quality);
          pmQualitySnapshotRef.current = quality;
        },
        onImageContextWarning: (payload) => {
          message.warning(
            payload?.message ||
              t(
                "chat.imageContextWarningDefault",
                "图片解析部分失败，系统将继续基于可用信息回答。",
              ),
          );
        },
        onSuperAssistantAnswer: (payload) => {
          markStreamActivity();
          if (payload.kind !== "deepAnalysis") return;
          const { link, taskId, status } = payload.answer;
          if (!taskId) return;
          resetPmResearchState();
          superAssistantAsyncTaskStartedRef.current = true;
          setPmBackgroundTaskId(taskId);
          setPmBackgroundTaskStatus(status || "queued");
          setPmPanelTaskId(taskId);
          setPmPanelTaskStatus(status || "queued");
          setPmPanelOpen((open) => (superAssistantEndpoint ? open : true));

          if (pmBackgroundTaskAbortRef.current) {
            pmBackgroundTaskAbortRef.current();
            pmBackgroundTaskAbortRef.current = null;
          }

          if (link === "pmResearchTask") {
            pmUserMessageIdByTaskIdRef.current[taskId] = userMsg.id;
            pmBackgroundTaskAbortRef.current = streamPmResearchTask(taskId, {
              onEvent: (event) => applyPmTaskEvent(event),
              onAnswerDelta: (event) => {
                appendPmAnswerDelta(event.delta);
              },
              onImageContextWarning: handlePmImageContextWarning,
              onDone: (event) => {
                applyPmTaskEvent(event);
                flushVisibleStreamingText();
                const doneStatus = derivePmBackgroundTaskStatus(event);
                setPmBackgroundTaskStatus(doneStatus);
                setPmPanelTaskStatus(doneStatus);
                pmBackgroundTaskAbortRef.current = null;
                appendPmTerminalMessageIfNeeded(event);
              },
              onError: (err) => {
                setPmBackgroundTaskStatus("failed");
                setPmPanelTaskStatus("failed");
                pmBackgroundTaskAbortRef.current = null;
                resetStreamingText();
                setIsStreaming(false);
                onStreamingChange?.(false);
                message.error(`${t("chat.streamError")}: ${err}`);
              },
            });
            return;
          }

          if (link === "chatAdversarialRun") {
            pmBackgroundTaskAbortRef.current =
              streamSuperAssistantAdversarialRun(taskId);
            return;
          }

          if (link === "dataAttributionTask") {
            pmBackgroundTaskAbortRef.current = streamNl2sqlAttributionTask(
              taskId,
              {
                onEvent: (event) => {
                  applyAttributionTaskEvent(event);
                },
                onDone: (event) => {
                  applyAttributionTaskEvent(event);
                  pmBackgroundTaskAbortRef.current = null;
                  const doneStatus = normalizeAttributionTaskStatus(
                    event.status,
                  );
                  setPmBackgroundTaskStatus(doneStatus);
                  setPmPanelTaskStatus(doneStatus);
                  setIsStreaming(false);
                  onStreamingChange?.(false);
                  appendAttributionTerminalMessageIfNeeded(event);
                },
                onError: (err) => {
                  setPmBackgroundTaskStatus("failed");
                  setPmPanelTaskStatus("failed");
                  pmBackgroundTaskAbortRef.current = null;
                  setIsStreaming(false);
                  onStreamingChange?.(false);
                  message.error(`${t("chat.streamError")}: ${err}`);
                },
              },
            );
          }
        },
        onStreamEnd: (
          _iterations: number,
          _usage: any,
          fullText?: string,
          finalThinking?: string,
          meta?: {
            pm_quality?: PmQualitySnapshot;
            pm_report?: PmReportArtifact;
            streamMode?: string;
            telemetry?: { cancelled?: boolean };
          },
        ) => {
          markStreamActivity();
          if (streamHandlersRef.current?.sessionId === sessionId) {
            streamHandlersRef.current = null;
          }
          setApprovalResolvingId(null);
          approvalPausedRef.current = null;
          setApprovalPaused(null);
          if (superAssistantEndpoint) {
            superAssistantTurnIdRef.current = null;
          }
          if (meta?.telemetry?.cancelled || meta?.streamMode === "cancelled") {
            superAssistantAsyncTaskStartedRef.current = false;
            resetStreamingText();
            setToolCalls({});
            toolCallsRef.current = {};
            liveToolIndicesRef.current = new Set();
            liveToolKeyByIndexRef.current = {};
            setIsStreaming(false);
            onStreamingChange?.(false);
            resetThinkingState();
            return;
          }
          const resolvedThinking = finalThinking ?? thinkingTextRef.current;
          const allToolEntries = Object.entries(toolCallsRef.current ?? {});
          const allToolCalls = allToolEntries.map(([, value]) => value);
          const completedToolCalls = allToolCalls.map((tc) =>
            tc.status === "pending" || tc.status === "running"
              ? {
                  ...tc,
                  status: "success" as const,
                }
              : tc,
          );

          // Close out the reasoning timer if the stream ended before any
          // explicit `thinking_end` / `text_delta` transition fired (e.g.
          // providers that jump straight from reasoning to finish).
          if (thinkingStartedAtRef.current != null) {
            thinkingDurationRef.current =
              Date.now() - thinkingStartedAtRef.current;
            thinkingStartedAtRef.current = null;
          }

          const pmQuality = normalizePmQualitySnapshot(meta?.pm_quality);
          if (pmQuality) {
            setPmQualitySnapshot(pmQuality);
            pmQualitySnapshotRef.current = pmQuality;
          }
          const pmReport = normalizePmReportArtifact(meta?.pm_report);
          const streamedAssistantText = (
            fullText ||
            streamingTextRef.current ||
            ""
          ).trim();
          if (
            superAssistantEndpoint &&
            superAssistantAsyncTaskStartedRef.current &&
            !streamedAssistantText
          ) {
            streamCommittedRef.current = true;
            superAssistantAsyncTaskStartedRef.current = false;
            resetStreamingText();
            pmActiveInlineSegmentIdRef.current = null;
            setToolCalls({});
            toolCallsRef.current = {};
            liveToolIndicesRef.current = new Set();
            liveToolKeyByIndexRef.current = {};
            // The short routing stream is done, but the spawned PM/adversarial/
            // attribution task is still the active answer for this session. Keep
            // the session in a busy state until that task emits its terminal event;
            // otherwise the user can send a follow-up that races ahead of the
            // final report and makes history appear out of order after refresh.
            setIsStreaming(true);
            onStreamingChange?.(true);
            resetThinkingState();
            return;
          }
          const assistantText =
            fullText || streamingTextRef.current || t("chat.noResponse");
          if (assistantText !== streamingTextRef.current) {
            streamingTextRef.current = assistantText;
            setStreamingText(assistantText);
            scheduleTypewriterDrain();
          }
          const evidenceSources = evidenceSourcesFromTurn(
            assistantText,
            completedToolCalls,
            evidenceDocuments,
          );
          const assistantMsg: DisplayMessage = {
            id: `asst-${Date.now()}`,
            role: "assistant",
            content: assistantText,
            timestamp: Date.now(),
            modelName: activeResponseModelNameRef.current || undefined,
            ...activeAdversarialMetaRef.current,
            attributionTaskId: isNl2sqlAttributionTaskId(
              pmBackgroundTaskIdRef.current,
            )
              ? pmBackgroundTaskIdRef.current || undefined
              : undefined,
            toolCalls:
              completedToolCalls.length > 0 ? completedToolCalls : undefined,
            evidenceSources:
              evidenceSources.length > 0 ? evidenceSources : undefined,
            thinking: resolvedThinking || null,
            // Persist the duration so the historical message renders
            // "已深度思考 · Xs" instead of a bare label. `undefined` is a
            // valid value — the bubble falls back to a duration-less
            // done state for it.
            thinkingDurationMs: resolvedThinking
              ? thinkingDurationRef.current
              : undefined,
            pmReport,
          };

          const commitAssistantMessage = () => {
            streamCommittedRef.current = true;
            setDisplayMessages((prev) => [...prev, assistantMsg]);
            resetStreamingText();
            pmActiveInlineSegmentIdRef.current = null;
            setToolCalls({});
            toolCallsRef.current = {};
            liveToolIndicesRef.current = new Set();
            liveToolKeyByIndexRef.current = {};
            setIsStreaming(false);
            onStreamingChange?.(false);
            // Clear live reasoning state. The persisted message has its
            // own copy (with the frozen duration) so the streaming-bubble
            // residue can be torn down immediately.
            thinkingLoadingRef.current = false;
            setThinkingLoading(false);
            setThinkingText("");
            thinkingTextRef.current = "";
            setThinkingDurationMs(undefined);
            thinkingDurationRef.current = undefined;
            superAssistantAsyncTaskStartedRef.current = false;
            const completedAt = Date.now();
            setPmStageStates((prev) => {
              const normalized = Object.fromEntries(
                Object.entries(prev).map(([key, value]) => [
                  key,
                  value.status === "running" || value.status === "pending"
                    ? {
                        ...value,
                        status: "completed" as const,
                        updatedAt: completedAt,
                        detail: {
                          ...(value.detail && typeof value.detail === "object"
                            ? (value.detail as Record<string, unknown>)
                            : {}),
                          terminalClosure: true,
                          message: t(
                            "operations.pmStageClosedByTerminalSuccess",
                            "任务已完成，阶段已收口",
                          ),
                        },
                      }
                    : value,
                ]),
              );
              pmStageStatesRef.current = normalized;
              return normalized;
            });

            onStreamFinished?.(
              assistantMsg,
              completedToolCalls,
              resolvedThinking,
            );
            void refreshSessionMemorySources(sessionId!, turnStartedAtMs);
            if (sessionSource === "chat") {
              void refreshLatestChatArtifacts(sessionId!, assistantMsg.id);
            }
          };

          flushVisibleStreamingText();
          commitAssistantMessage();
        },
        onError: (error: string) => {
          markStreamActivity();
          if (streamHandlersRef.current?.sessionId === sessionId) {
            streamHandlersRef.current = null;
          }
          superAssistantAsyncTaskStartedRef.current = false;
          const adversarialNeedsModels =
            isSuperAdversarialNeedsModelsError(error);
          if (adversarialNeedsModels) {
            message.warning(t("chat.adversarialNeedModels"));
          } else if (error.includes("session busy") || error.includes("SessionBusy")) {
            message.warning({ content: t("chat.sessionBusy"), duration: 6 });
          } else if (
            error.includes("empty response from model") ||
            error.includes("all API keys failed")
          ) {
            message.error(
              t(
                "chat.upstreamEmptyResponse",
                "模型服务返回空响应或密钥链路不可用。系统已自动重试，若仍失败请检查模型路由与可用密钥。",
              ),
            );
          } else if (
            error.includes("network request failures across all tool calls") ||
            error.includes("web search network failure on all endpoints") ||
            error.includes("web search unavailable on all endpoints")
          ) {
            message.error(
              t(
                "chat.pmNetworkBlocked",
                "检索网络链路异常（服务端请求被拦截或不可达），系统已快速失败。请切换可用检索源或代理后重试。",
              ),
            );
          } else {
            message.error(`${t("chat.streamError")}: ${error}`);
          }
          const partialText = streamingTextRef.current;
          const partialThinking = thinkingTextRef.current;
          const allToolEntries = Object.entries(toolCallsRef.current ?? {});
          const allToolCalls = allToolEntries.map(([, value]) => value);
          const completedToolCalls = allToolCalls.map((tc) =>
            tc.status === "pending" || tc.status === "running"
              ? {
                  ...tc,
                  status: tc.isError
                    ? ("error" as const)
                    : ("success" as const),
                }
              : tc,
          );
          const failureText = adversarialNeedsModels
            ? t("chat.adversarialNeedModels")
            : error.includes("complete_turn")
              ? "本次回答已完成检索，但未通过最终完整性校验。系统已停止重复执行，请重新发送后再试。"
              : error.includes("流提前结束") || error.includes("stream_end")
                ? "连接中断且自动恢复失败，本次回答未能完整返回。"
                : `本次回答未能完成：${shortHumanText(error, 320)}`;
          if (!streamCommittedRef.current) {
            const assistantMsg: DisplayMessage = {
              id: `asst-${Date.now()}`,
              role: "assistant",
              content: partialText || failureText,
              timestamp: Date.now(),
              modelName: activeResponseModelNameRef.current || undefined,
              ...activeAdversarialMetaRef.current,
              toolCalls:
                completedToolCalls.length > 0 ? completedToolCalls : undefined,
              evidenceSources: evidenceSourcesFromTurn(
                partialText,
                completedToolCalls,
                evidenceDocuments,
              ),
              thinking: partialThinking || null,
              thinkingDurationMs: partialThinking
                ? thinkingDurationRef.current
                : undefined,
            };
            flushVisibleStreamingText();
            streamCommittedRef.current = true;
            setDisplayMessages((prev) => [...prev, assistantMsg]);
            onStreamFinished?.(
              assistantMsg,
              completedToolCalls,
              partialThinking,
            );
            void refreshSessionMemorySources(sessionId!, turnStartedAtMs);
            if (sessionSource === "chat") {
              void refreshLatestChatArtifacts(sessionId!, assistantMsg.id);
            }
          }
          resetStreamingText();
          setToolCalls({});
          toolCallsRef.current = {};
          liveToolIndicesRef.current = new Set();
          liveToolKeyByIndexRef.current = {};
          setIsStreaming(false);
          onStreamingChange?.(false);
          // Drop any half-captured reasoning state; leaving it would
          // show a bogus "思考中…" bubble attached to nothing.
          resetThinkingState();
        },
      };
    streamHandlersRef.current = { sessionId: sessionId!, handlers: streamHandlers };
    const abort = streamAgentSession(
      sessionId!,
      finalMessage,
      streamHandlers,
      superAssistantEndpoint
        ? {
            images: streamImages,
            documents: streamDocuments,
            turnOptions,
            superAssistant: {
              app: "chat",
              ...(effectiveTurnModel ? { model: effectiveTurnModel } : {}),
              ...(superAssistantSlash ? { displayText: rawInput } : {}),
              ...slashRequestOptions,
            },
          }
        : { images: streamImages, documents: streamDocuments, turnOptions },
    );
    abortRef.current = abort;
  };

  // ── Keyboard ────────────────────────────────────────────────────────────────────────────────
  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (isComposingRef.current) return;

    if (slashOpen) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSlashSelected((s) =>
          Math.min(
            s + 1,
            filterSlashCommands(allSlashCommands, slashFilter).length - 1,
          ),
        );
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setSlashSelected((s) => Math.max(s - 1, 0));
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        const cmds = filterSlashCommands(allSlashCommands, slashFilter);
        if (cmds[slashSelected]) setInput(`/${cmds[slashSelected].name} `);
        setSlashOpen(false);
        setSlashFilter("");
        setSlashSelected(0);
        composerRef.current?.focus();
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setSlashOpen(false);
        setSlashFilter("");
        setSlashSelected(0);
        return;
      }
      return;
    }

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleInputChange = (val: string) => {
    syncInputValue(val);
    const slashMatch = val.match(/^(\/\S*)$/);
    if (slashMatch) {
      setSlashFilter(slashMatch[1].slice(1));
      setSlashOpen(true);
      setSlashSelected(0);
    } else if (slashOpen) {
      setSlashOpen(false);
      setSlashFilter("");
    }
  };

  const handleStop = async () => {
    if (
      !superAssistantEndpoint &&
      pmBackgroundTaskId &&
      !isPmTaskTerminalStatus(pmBackgroundTaskStatus)
    ) {
      await cancelPmBackgroundResearch();
    }
    await stopActiveTurn(activeSessionId);
  };

  const applyPmFollowupPrompt = useCallback(
    (mode: "repair" | "challenge") => {
      if (mode === "repair") {
        setInput(
          t(
            "operations.pmPromptRepair",
            "请继续自动检索并修复证据不足项：每条关键结论都补充来源URL，至少覆盖2个域名；不确定项单独列出。",
          ),
        );
      } else {
        setInput(
          t(
            "operations.pmPromptChallenge",
            "请对当前结论做反证检查：补充相反证据、说明冲突点，并给出最终裁决与依据URL。",
          ),
        );
      }
      setTimeout(() => composerRef.current?.focus(), 0);
    },
    [t],
  );

  const refreshPmSubtaskRuntime = useCallback(
    async (taskId: string, force = false) => {
      if (!taskId) return;
      const now = Date.now();
      if (!force && now - pmSubtaskLastRefreshAtRef.current < 1200) {
        return;
      }
      pmSubtaskLastRefreshAtRef.current = now;
      try {
        const subtaskResp = await agentApi.getPmResearchTaskSubtasks(taskId);
        const rows = Array.isArray(subtaskResp.items) ? subtaskResp.items : [];
        setPmSubtaskRows(rows);
        const targetKeys = rows
          .slice(0, 8)
          .map((row) => row.subtask_id || row.subtask_key)
          .filter(
            (value): value is string => !!value && value.trim().length > 0,
          );
        if (targetKeys.length === 0) {
          setPmSubtaskAttempts({});
          return;
        }
        const entries = await Promise.all(
          targetKeys.map(async (key) => {
            try {
              const attemptResp =
                await agentApi.getPmResearchTaskSubtaskAttempts(taskId, key);
              return [
                key,
                Array.isArray(attemptResp.items) ? attemptResp.items : [],
              ] as const;
            } catch {
              return [key, []] as const;
            }
          }),
        );
        setPmSubtaskAttempts(Object.fromEntries(entries));
      } catch {
        // best-effort runtime panel enhancement
      }
    },
    [],
  );

  const finalizePmRunningStages = useCallback(
    (terminalStatus: string, at = Date.now()) => {
      const failed =
        terminalStatus === "failed" || terminalStatus === "cancelled";
      const finalStageStatus: PmStageStatus = failed ? "failed" : "completed";
      setPmStageStates((prev) => {
        const merged: Record<string, PmStageState> = { ...prev };
        let changed = false;
        for (const [key, value] of Object.entries(merged)) {
          if (value.status === "running" || value.status === "pending") {
            const closedStatus: PmStageStatus = failed
              ? "failed"
              : value.status === "pending"
                ? "skipped"
                : "completed";
            merged[key] = {
              ...value,
              status: closedStatus,
              updatedAt: at,
              detail: {
                ...(value.detail && typeof value.detail === "object"
                  ? (value.detail as Record<string, unknown>)
                  : {}),
                terminalTaskStatus: terminalStatus,
                message: failed
                  ? t(
                      "operations.pmStageClosedByTerminalFailure",
                      "任务已结束，阶段已停止",
                    )
                  : t(
                      "operations.pmStageClosedByTerminalSuccess",
                      "任务已完成，阶段已收口",
                    ),
              },
            };
            changed = true;
          }
        }
        if (!changed) return prev;
        pmStageStatesRef.current = merged;
        return merged;
      });
      setPmStageEvents((prev) => {
        const synthetic = Object.values(pmStageStatesRef.current)
          .filter(
            (stage) => stage.status === "running" || stage.status === "pending",
          )
          .map((stage) => {
            const closedStatus: PmStageStatus = failed
              ? "failed"
              : stage.status === "pending"
                ? "skipped"
                : finalStageStatus;
            return {
              stage: stage.stage,
              status: closedStatus,
              attempt: stage.attempt,
              detail: {
                ...(stage.detail && typeof stage.detail === "object"
                  ? (stage.detail as Record<string, unknown>)
                  : {}),
                terminalTaskStatus: terminalStatus,
                message: failed
                  ? t(
                      "operations.pmStageClosedByTerminalFailure",
                      "任务已结束，阶段已停止",
                    )
                  : t(
                      "operations.pmStageClosedByTerminalSuccess",
                      "任务已完成，阶段已收口",
                    ),
              },
              at,
            };
          });
        if (synthetic.length === 0) return prev;
        const merged = [...prev, ...synthetic].slice(-120);
        pmStageEventsRef.current = merged;
        return merged;
      });
      pmActiveInlineSegmentIdRef.current = null;
    },
    [t],
  );

  const applyPmTaskEvent = useCallback(
    (event: ApiPmResearchTaskEvent, options?: { replay?: boolean }) => {
      const nextStatus = derivePmBackgroundTaskStatus(event);
      setPmBackgroundTaskId(event.task_id);
      setPmBackgroundTaskStatus(nextStatus);
      setPmPanelTaskId(event.task_id);
      setPmPanelTaskStatus(nextStatus);

      const stageName = normalizePmTaskEventStage(event);
      const status = normalizePmTaskEventStageStatus(event);
      const attempt =
        typeof event.attempt === "number" && event.attempt > 0
          ? event.attempt
          : 1;
      const at = Date.now();
      const rawIncomingDetail = normalizePmTaskEventDetail(event);
      const normalizedDetail = (() => {
        if (!rawIncomingDetail?.liveToolEvent) return rawIncomingDetail;
        const evt = parsePmLiveToolEvent(rawIncomingDetail);
        if (evt?.phase !== "start") return rawIncomingDetail;
        const liveTarget =
          evt.target ??
          extractPmTargetFromDetail(rawIncomingDetail) ??
          shortHumanText(evt.tool, 96);
        return {
          ...rawIncomingDetail,
          message: `${t("chat.toolStatusRunning", "正在")}搜索: 「${liveTarget}」`,
        };
      })();
      const previousStageState = pmStageStatesRef.current[stageName];
      const isStaleStageRegression = (() => {
        if (!previousStageState) return false;
        if (attempt < previousStageState.attempt) return true;
        if (attempt > previousStageState.attempt) return false;
        const prevTerminal = isPmStageTerminal(previousStageState.status);
        const nextNonTerminal = status === "running" || status === "pending";
        return prevTerminal && nextNonTerminal;
      })();
      if (isStaleStageRegression) {
        return;
      }
      const runningSince =
        status === "running"
          ? previousStageState?.status === "running" &&
            previousStageState.attempt === attempt
            ? (previousStageState.runningSince ?? previousStageState.updatedAt)
            : at
          : previousStageState?.status === "running" &&
              previousStageState.attempt === attempt
            ? (previousStageState.runningSince ?? previousStageState.updatedAt)
            : previousStageState?.runningSince;
      const nextEntry: PmStageState = {
        stage: stageName,
        status,
        attempt,
        detail: normalizedDetail,
        runningSince,
        updatedAt: at,
      };
      setPmStageStates((prev) => {
        const merged = { ...prev, [stageName]: nextEntry };
        pmStageStatesRef.current = merged;
        return merged;
      });
      if (stageName === "retry_repair" && status === "running") {
        const existing = pmStageStatesRef.current["retrieve"];
        if (existing?.status === "failed") {
          const merged = {
            ...pmStageStatesRef.current,
            retrieve: {
              ...existing,
              status: "running" as const,
              updatedAt: at,
              detail: {
                ...(existing.detail && typeof existing.detail === "object"
                  ? (existing.detail as Record<string, unknown>)
                  : {}),
                message: t(
                  "operations.pmRetrieveRepairing",
                  "自动修复中：正在切换来源并继续检索",
                ),
              },
            },
          };
          pmStageStatesRef.current = merged;
          setPmStageStates(merged);
        }
      }
      setPmStageEvents((prev) => {
        const merged = [
          ...prev,
          {
            stage: stageName,
            status,
            attempt,
            detail: normalizedDetail,
            at,
          },
        ].slice(-120);
        pmStageEventsRef.current = merged;
        return merged;
      });
      const summary = buildPmStageNarrative(
        stageName,
        status,
        normalizedDetail,
      );
      const rawDetail = normalizedDetail;
      const segmentId = ensurePmInlineSegment(
        stageName,
        status,
        attempt,
        summary,
        rawDetail,
      );
      if (isPmLightweightChatDetail(rawDetail)) {
        setPmSuppressExecutionUi(true);
        setPmPanelOpen(false);
      }
      hydratePmInlineFromStageDetail(segmentId, stageName, rawDetail);
      const isRetrieveRunningTransition =
        stageName === "retrieve" &&
        status === "running" &&
        !(
          previousStageState?.status === "running" &&
          previousStageState.attempt === attempt
        );
      if (isRetrieveRunningTransition) {
        liveToolIndicesRef.current = new Set();
        liveToolKeyByIndexRef.current = {};
      }
      if (status === "running") {
        pmActiveInlineSegmentIdRef.current = segmentId;
      } else if (pmActiveInlineSegmentIdRef.current === segmentId) {
        pmActiveInlineSegmentIdRef.current = null;
      }

      if (stageName === "understand" && rawDetail) {
        const thinkingPreview = pickPmDetailString(rawDetail, "thinking");
        if (thinkingPreview) {
          thinkingTextRef.current = thinkingPreview;
          setThinkingText(thinkingPreview);
        }
      }

      const responseAny = event.response as
        | {
            pm_quality?: PmQualitySnapshot;
          }
        | undefined;
      const quality = normalizePmQualitySnapshot(responseAny?.pm_quality);
      if (quality) {
        setPmQualitySnapshot(quality);
        pmQualitySnapshotRef.current = quality;
      }
      const shouldRefreshSubtasks =
        stageName === "retrieve" ||
        stageName === "verify" ||
        stageName === "synthesize" ||
        stageName === "subtask_started" ||
        stageName === "subtask_completed" ||
        stageName === "subtask_failed" ||
        stageName === "merge_started" ||
        stageName === "merge_completed";
      if (shouldRefreshSubtasks && event.task_id) {
        void refreshPmSubtaskRuntime(
          event.task_id,
          status === "completed" || status === "failed",
        );
      }
      if (
        sessionSource === "pm" &&
        !options?.replay &&
        event.status === "completed" &&
        quality
      ) {
        const retrieveState = pmStageStatesRef.current.retrieve;
        const routeInfo = pickRetrieveRouteInfo(
          retrieveState?.detail as Record<string, unknown> | undefined,
        );
        const runKey = [
          "bg",
          event.task_id,
          routeInfo.route,
          routeInfo.channel ?? "na",
          String(event.elapsed_ms ?? 0),
        ].join(":");
        recordPmStrategyOutcome({
          key: runKey,
          at: Date.now(),
          sessionId: event.session_id,
          route: routeInfo.route,
          channel: routeInfo.channel,
          variant: routeInfo.variant,
          passed: !!quality.passed,
          citationCount: quality.citation_count ?? 0,
          domainCount: quality.domain_count ?? 0,
          toolCallCount: quality.tool_call_count ?? 0,
          retrieveDurationMs: routeInfo.durationMs,
        });
      }
      if (isPmTaskTerminalEvent(event)) {
        finalizePmRunningStages(nextStatus, at);
      }
    },
    [
      buildPmStageNarrative,
      ensurePmInlineSegment,
      finalizePmRunningStages,
      hydratePmInlineFromStageDetail,
      recordPmStrategyOutcome,
      refreshPmSubtaskRuntime,
      sessionSource,
    ],
  );

  const appendPmTerminalMessageIfNeeded = useCallback(
    (event: ApiPmResearchTaskEvent) => {
      const responseAny = event.response as
        | {
            text?: string;
            pm_quality?: PmQualitySnapshot;
            pm_report?: unknown;
          }
        | undefined;
      const pmQuality = normalizePmQualitySnapshot(responseAny?.pm_quality);
      if (pmQuality) {
        setPmQualitySnapshot(pmQuality);
        pmQualitySnapshotRef.current = pmQuality;
      }

      const pmReport = normalizePmReportArtifact(responseAny?.pm_report);
      const pmFinalDelivery: PmFinalDeliveryArtifact | undefined =
        event.task_id && responseAny
          ? {
              schemaVersion: "pm-final-delivery-v1",
              taskId: event.task_id,
              taskStatus: event.status,
              qualityStatus: pmQuality?.passed ? "passed" : "degraded",
              deliveryStatus: "persisted",
              response: {
                ...responseAny,
                pm_report: pmReport,
              },
              stages: Object.values(pmStageStatesRef.current).map(
                (stage, index) => ({
                  stage: stage.stage,
                  status: stage.status,
                  attempt: stage.attempt,
                  detail: stage.detail,
                  lastEventSeq: index + 1,
                  updatedAt: new Date(stage.updatedAt).toISOString(),
                }),
              ),
              contentHash: event.task_id,
            }
          : undefined;
      const finalText = resolvePmTerminalMessageText(
        event,
        t("chat.unknownError", "未知错误"),
      );
      if (!finalText.trim()) {
        streamCommittedRef.current = true;
        resetStreamingText();
        setIsStreaming(false);
        onStreamingChange?.(false);
        return;
      }
      if (streamingTextRef.current.trim()) {
        streamingTextRef.current = finalText;
        setStreamingText(finalText);
        flushVisibleStreamingText();
        resetStreamingText();
      }
      streamCommittedRef.current = true;
      const pmArtifacts = pmStageEventsToMessageArtifacts(
        pmStageEventsRef.current,
      );
      setDisplayMessages((prev) => {
        const reconciled = reconcilePmHistoryTerminalAssistant(
          prev,
          finalText,
          {
            taskId: event.task_id,
            taskStatus: event.status,
            pmReport,
            pmFinalDelivery,
            userMessageId: event.task_id
              ? pmUserMessageIdByTaskIdRef.current[event.task_id]
              : undefined,
          },
        );
        return attachPmSearchUsageToLatestAssistant(
          reconciled,
          pmArtifacts.pmSearchUsage,
          pmArtifacts.traceEvents,
        );
      });
      setIsStreaming(false);
      onStreamingChange?.(false);
      setTimeout(scrollToBottom, 30);
    },
    [
      flushVisibleStreamingText,
      onStreamingChange,
      resetStreamingText,
      scrollToBottom,
      t,
    ],
  );

  const appendPmAnswerDelta = useCallback(
    (delta?: string | null) => {
      if (!delta) return;
      if (!isStreaming) {
        setStreamingMessageTimestamp((prev) => prev ?? Date.now());
        setIsStreaming(true);
        onStreamingChange?.(true);
      }
      const next = streamingTextRef.current + delta;
      streamingTextRef.current = next;
      setStreamingText(next);
      scheduleTypewriterDrain();
      setTimeout(scrollToBottom, 10);
    },
    [isStreaming, onStreamingChange, scheduleTypewriterDrain, scrollToBottom],
  );

  const enqueuePmPrompt = useCallback(
    (
      item: PmQueuedPromptDraft,
      options?: { front?: boolean; showToast?: boolean },
    ) => {
      const id = `pm-queued-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
      const next: PmQueuedPrompt = {
        ...item,
        id,
        userMessageId: item.userMessageId || `user-${id}`,
        createdAt: Date.now(),
      };
      const merged = options?.front
        ? [next, ...pmPromptQueueRef.current]
        : [...pmPromptQueueRef.current, next];
      commitPmPromptQueue(merged);
      if (options?.showToast !== false) {
        message.info(
          t(
            "operations.pmQueuedPromptAdded",
            "当前研究任务运行中，消息已加入待执行队列",
          ),
        );
      }
      return next.id;
    },
    [commitPmPromptQueue, t],
  );

  const startPmQueuedPrompt = useCallback(
    async (
      queued: PmQueuedPrompt,
      sessionId: string,
      options?: { appendUserMessage?: boolean },
    ) => {
      pmQueueStartingRef.current = true;
      const shouldAppendUserMessage =
        options?.appendUserMessage ?? queued.appendUserMessage ?? true;
      if (shouldAppendUserMessage) {
        const userMsgId = queued.userMessageId;
        if (!pmQueuedUserMessageIdsRef.current.has(userMsgId)) {
          pmQueuedUserMessageIdsRef.current.add(userMsgId);
          const userMsg: DisplayMessage = {
            id: userMsgId,
            role: "user",
            content: queued.content,
            timestamp: queued.createdAt,
            replyTo: queued.replyTo,
          };
          setDisplayMessages((prev) => [...prev, userMsg]);
          onBeforeStream?.(userMsg);
          autoFollowScrollRef.current = true;
          setTimeout(() => scrollToBottom(true), 30);
        }
      }

      resetPmResearchState();
      setPmBackgroundTaskStatus("queued");
      setPmPanelTaskStatus("queued");
      try {
        const start = await agentApi.startPmResearchTask(
          sessionId,
          queued.finalMessage,
          queued.images,
          queued.documents,
        );
        const taskId = start.taskId ?? start.task_id;
        if (!taskId) {
          throw new Error("missing task_id");
        }
        pmUserMessageIdByTaskIdRef.current[taskId] = queued.userMessageId;
        setPmBackgroundTaskId(taskId);
        const nextStatus = start.status ?? "queued";
        setPmBackgroundTaskStatus(nextStatus);
        setPmPanelTaskId(taskId);
        setPmPanelTaskStatus(nextStatus);
        pmBackgroundTaskAbortRef.current = streamPmResearchTask(taskId, {
          onEvent: (event) => {
            applyPmTaskEvent(event);
          },
          onAnswerDelta: (event) => {
            appendPmAnswerDelta(event.delta);
          },
          onImageContextWarning: handlePmImageContextWarning,
          onDone: (event) => {
            applyPmTaskEvent(event);
            flushVisibleStreamingText();
            const doneStatus = derivePmBackgroundTaskStatus(event);
            setPmBackgroundTaskStatus(doneStatus);
            setPmPanelTaskStatus(doneStatus);
            pmBackgroundTaskAbortRef.current = null;
            pmQueueStartingRef.current = false;
            appendPmTerminalMessageIfNeeded(event);
          },
          onError: (err) => {
            setPmBackgroundTaskStatus("failed");
            setPmPanelTaskStatus("failed");
            pmBackgroundTaskAbortRef.current = null;
            pmQueueStartingRef.current = false;
            resetStreamingText();
            setIsStreaming(false);
            onStreamingChange?.(false);
            message.error(`${t("chat.streamError")}: ${err}`);
          },
        });
      } catch (err) {
        setPmBackgroundTaskStatus(null);
        setPmBackgroundTaskId(null);
        setPmPanelTaskStatus(null);
        setPmPanelTaskId(null);
        pmQueueStartingRef.current = false;
        message.error(`${t("chat.streamError")}: ${(err as Error).message}`);
        throw err;
      }
    },
    [
      appendPmTerminalMessageIfNeeded,
      appendPmAnswerDelta,
      applyPmTaskEvent,
      flushVisibleStreamingText,
      handlePmImageContextWarning,
      onBeforeStream,
      onStreamingChange,
      resetPmResearchState,
      resetStreamingText,
      scrollToBottom,
      t,
    ],
  );

  const openPmExecutionPanelForMessage = useCallback(
    (msg: DisplayMessage) => {
      if (sessionSource !== "pm" || msg.role !== "assistant") return;
      const taskId = msg.pmTaskId;
      if (!taskId) {
        setPmPanelOpen(true);
        return;
      }

      const activeStatus = (pmBackgroundTaskStatus ?? "").toLowerCase();
      const activeIsRunning =
        activeStatus === "queued" ||
        activeStatus === "running" ||
        activeStatus === "cancelling";
      if (
        activeIsRunning &&
        pmBackgroundTaskId &&
        pmBackgroundTaskId !== taskId
      ) {
        message.warning(
          t(
            "operations.pmPanelTaskSwitchBlocked",
            "当前有研究任务运行中，请等待完成后再查看其他回复的执行面板。",
          ),
        );
        return;
      }

      const sameTask = pmBackgroundTaskId === taskId;
      const hasPanelState =
        Object.keys(pmStageStatesRef.current).length > 0 ||
        pmStageEventsRef.current.length > 0;
      if (sameTask && hasPanelState) {
        setPmPanelTaskId(taskId);
        setPmPanelTaskStatus(
          msg.pmTaskStatus ?? pmBackgroundTaskStatus ?? null,
        );
        setPmPanelOpen(true);
        void refreshPmSubtaskRuntime(taskId, true);
        return;
      }

      if (pmPanelReplayAbortRef.current) {
        pmPanelReplayAbortRef.current();
        pmPanelReplayAbortRef.current = null;
      }

      resetPmResearchState();
      setPmPanelTaskId(taskId);
      setPmPanelTaskStatus(msg.pmTaskStatus ?? null);
      setPmBackgroundTaskId(taskId);
      setPmBackgroundTaskStatus("running");
      setPmPanelOpen(true);
      void refreshPmSubtaskRuntime(taskId, true);

      pmPanelReplayAbortRef.current = streamPmResearchTask(taskId, {
        onEvent: (event) => {
          applyPmTaskEvent(event, { replay: true });
        },
        onImageContextWarning: handlePmImageContextWarning,
        onDone: (event) => {
          applyPmTaskEvent(event, { replay: true });
          const nextStatus = derivePmBackgroundTaskStatus(event);
          setPmBackgroundTaskStatus(nextStatus);
          setPmPanelTaskStatus(nextStatus);
          pmPanelReplayAbortRef.current = null;
        },
        onError: (err) => {
          pmPanelReplayAbortRef.current = null;
          setPmBackgroundTaskStatus("failed");
          setPmPanelTaskStatus("failed");
          message.error(`${t("chat.streamError")}: ${err}`);
        },
      });
    },
    [
      applyPmTaskEvent,
      handlePmImageContextWarning,
      pmBackgroundTaskId,
      pmBackgroundTaskStatus,
      refreshPmSubtaskRuntime,
      resetPmResearchState,
      sessionSource,
      t,
    ],
  );

  const openPmSharePreviewForMessage = useCallback(
    (msg: DisplayMessage) => {
      if (msg.role !== "assistant") return;
      const payload = buildPmSharePreviewPayload(msg);
      if (!payload) {
        message.warning(
          t(
            "operations.pmReplySharePreviewNoContent",
            "当前回复暂无可预览内容",
          ),
        );
        return;
      }
      try {
        const shareUrl = buildPmSharePreviewUrl(payload);
        window.open(shareUrl, "_blank", "noopener,noreferrer");
      } catch {
        message.error(
          t("operations.pmReplySharePreviewFailed", "打开预览页面失败"),
        );
      }
    },
    [t],
  );

  const startPmBackgroundResearch = useCallback(async () => {
    if (sessionSource !== "pm") return;
    if (isStreaming) {
      message.warning(
        t(
          "operations.pmBackgroundBlockedByStreaming",
          "当前前台正在执行，请先停止或等待完成。",
        ),
      );
      return;
    }
    const pmAttachmentMeta = collectPmTaskAttachments(attachments);
    if (
      pmAttachmentMeta.hasUnsupportedImageSource ||
      pmAttachmentMeta.hasUnsupportedDocumentSource
    ) {
      message.warning(
        t(
          "operations.pmBackgroundImageSourceUnsupported",
          "后台研究仅支持已上传附件，请重新上传后再试。",
        ),
      );
      return;
    }
    if (pmAttachmentMeta.images.length > PM_MAX_IMAGE_ATTACHMENTS) {
      message.warning(
        t(
          "operations.pmBackgroundImageLimit",
          "后台研究最多支持 5 张图片，请减少附件后重试。",
        ),
      );
      return;
    }
    const raw = draftInputRef.current.trim();
    if (
      !raw &&
      pmAttachmentMeta.images.length === 0 &&
      pmAttachmentMeta.documents.length === 0
    ) {
      message.warning(t("chat.emptyInput"));
      return;
    }

    const repliedPrefix = buildReplyPrefix(replyingTo, displayMessages);
    const visibleMessage = buildMessageContent(raw, attachments);
    const finalMessage = repliedPrefix + raw;
    const attachedDocuments = attachments.filter(
      (att): att is DocumentBlock => att.type === "document",
    );
    setInput("");
    setAttachments([]);
    setReplyingTo(null);

    let sessionId = activeSessionId;
    if (!sessionId) {
      try {
        const session = await createRuntimeSession();
        sessionId = session.session.session_id;
        setActiveSessionId(sessionId);
        setActiveMcpServers(sessionMetadataNames(session.session.mcp_servers));
        setActiveSkills(sessionMetadataNames(session.session.skills));
        onSessionCreated?.(sessionId);
        await qc.invalidateQueries({
          queryKey: queryKeys.agentSessions.list(runtimeSessionSource),
        });
      } catch (err) {
        message.error(`${t("chat.streamError")}: ${(err as Error).message}`);
        return;
      }
    }

    if (attachedDocuments.length > 0 && sessionId) {
      try {
        const indexed = await registerUploadedDocumentsForSession(
          attachedDocuments,
          sessionId,
        );
        if (Object.keys(indexed).length > 0) {
          setChatFileRecords((prev) => ({ ...prev, ...indexed }));
        }
        const failed = Object.values(indexed).filter(
          (record) => record.status === "failed",
        );
        if (failed.length > 0) {
          message.warning(
            `${t("chat.fileIndexFailed", "File indexing failed")}: ${failed.map((record) => record.filename).join(", ")}`,
          );
        }
      } catch (err) {
        message.warning(
          `${t("chat.fileIndexFailed", "File indexing failed")}: ${(err as Error).message}`,
        );
      }
    }

    const queuedPrompt: PmQueuedPromptDraft = {
      content: visibleMessage,
      finalMessage,
      images: pmAttachmentMeta.images,
      documents: pmAttachmentMeta.documents,
      source: "input",
      appendUserMessage: true,
      replyTo: replyingTo ?? undefined,
    };
    const currentPmBusy =
      pmQueueStartingRef.current ||
      pmBackgroundTaskAbortRef.current !== null ||
      pmBackgroundTaskStatus === "queued" ||
      pmBackgroundTaskStatus === "running" ||
      pmBackgroundTaskStatus === "cancelling";

    if (currentPmBusy) {
      enqueuePmPrompt(queuedPrompt);
    } else {
      await startPmQueuedPrompt(
        {
          ...queuedPrompt,
          id: `pm-direct-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
          userMessageId: `user-pm-direct-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
          createdAt: Date.now(),
        },
        sessionId!,
      );
      message.success(
        t("operations.pmBackgroundStarted", "已启动后台研究任务"),
      );
    }
  }, [
    activeSessionId,
    attachments,
    displayMessages,
    enqueuePmPrompt,
    isStreaming,
    onSessionCreated,
    qc,
    replyingTo,
    pmBackgroundTaskStatus,
    sessionSource,
    startPmQueuedPrompt,
    t,
  ]);

  const cancelPmBackgroundResearch = useCallback(async () => {
    if (!pmBackgroundTaskId) return;
    try {
      if (isNl2sqlAttributionTaskId(pmBackgroundTaskId)) {
        const resp = await nl2sqlApi.cancelAttributionTask(pmBackgroundTaskId);
        const status = normalizeAttributionTaskStatus(resp.status);
        setPmBackgroundTaskStatus(status);
        setPmPanelTaskStatus(status);
      } else if (isChatAdversarialRunId(pmBackgroundTaskId)) {
        const resp =
          await agentApi.cancelChatAdversarialRun(pmBackgroundTaskId);
        const status = normalizeAdversarialRunStatus(resp.status);
        setPmBackgroundTaskStatus(status);
        setPmPanelTaskStatus(status);
      } else {
        await agentApi.cancelPmResearchTask(pmBackgroundTaskId);
        setPmBackgroundTaskStatus("cancelling");
        setPmPanelTaskStatus("cancelling");
      }
      message.info(
        t("operations.pmBackgroundCancelling", "已请求取消后台研究任务"),
      );
    } catch (err) {
      message.error(`${t("chat.streamError")}: ${(err as Error).message}`);
    }
  }, [pmBackgroundTaskId, t]);

  const movePmQueuedPrompt = useCallback(
    (id: string, direction: -1 | 1) => {
      const current = pmPromptQueueRef.current;
      const index = current.findIndex((item) => item.id === id);
      const nextIndex = index + direction;
      if (index < 0 || nextIndex < 0 || nextIndex >= current.length) return;
      const next = [...current];
      const [item] = next.splice(index, 1);
      next.splice(nextIndex, 0, item);
      commitPmPromptQueue(next);
    },
    [commitPmPromptQueue],
  );

  const removePmQueuedPrompt = useCallback(
    (id: string) => {
      commitPmPromptQueue(
        pmPromptQueueRef.current.filter((item) => item.id !== id),
      );
    },
    [commitPmPromptQueue],
  );

  const replaceCurrentPmBackgroundResearch = useCallback(async () => {
    if (sessionSource !== "pm") return;
    const raw = draftInputRef.current.trim();
    const hasDraft = raw.length > 0 || attachments.length > 0;
    const hasQueuedPrompt = pmPromptQueueRef.current.length > 0;
    if (!hasDraft && !hasQueuedPrompt) {
      message.warning(
        t(
          "operations.pmReplaceNeedsInputOrQueue",
          "请输入新消息，或先发送一条消息加入队列。",
        ),
      );
      return;
    }

    if (hasDraft) {
      const pmAttachmentMeta = collectPmTaskAttachments(attachments);
      if (
        pmAttachmentMeta.hasUnsupportedImageSource ||
        pmAttachmentMeta.hasUnsupportedDocumentSource
      ) {
        message.warning(
          t(
            "operations.pmBackgroundImageSourceUnsupported",
            "后台研究仅支持已上传附件，请重新上传后再试。",
          ),
        );
        return;
      }
      if (pmAttachmentMeta.images.length > PM_MAX_IMAGE_ATTACHMENTS) {
        message.warning(
          t(
            "operations.pmBackgroundImageLimit",
            "后台研究最多支持 5 张图片，请减少附件后重试。",
          ),
        );
        return;
      }
      const visibleMessage = buildMessageContent(raw, attachments);
      const repliedPrefix = buildReplyPrefix(replyingTo, displayMessages);
      enqueuePmPrompt(
        {
          content: visibleMessage,
          finalMessage: repliedPrefix + raw,
          images: pmAttachmentMeta.images,
          documents: pmAttachmentMeta.documents,
          source: "replace",
          appendUserMessage: true,
          replyTo: replyingTo ?? undefined,
        },
        { front: true, showToast: false },
      );
      setInput("");
      setAttachments([]);
      setReplyingTo(null);
    }

    if (pmBackgroundTaskId && !isPmTaskTerminalStatus(pmBackgroundTaskStatus)) {
      await cancelPmBackgroundResearch();
      message.info(
        hasDraft
          ? t(
              "operations.pmReplaceQueued",
              "已取消当前任务，替换消息将排在队首执行",
            )
          : t(
              "operations.pmReplaceWithQueueHead",
              "已取消当前任务，将执行队首消息",
            ),
      );
    }
  }, [
    attachments,
    cancelPmBackgroundResearch,
    displayMessages,
    enqueuePmPrompt,
    pmBackgroundTaskId,
    pmBackgroundTaskStatus,
    pmPromptQueue,
    replyingTo,
    sessionSource,
    t,
  ]);

  const resumePmBackgroundResearch = useCallback(async () => {
    if (!isPmResearchTaskId(pmBackgroundTaskId)) return;
    try {
      const resp = await agentApi.resumePmResearchTask(pmBackgroundTaskId);
      const newTaskId = resp.taskId ?? resp.task_id;
      if (!newTaskId) throw new Error("missing task_id");
      if (pmBackgroundTaskAbortRef.current) {
        pmBackgroundTaskAbortRef.current();
      }
      setPmBackgroundTaskId(newTaskId);
      const resumeStatus = resp.status ?? "queued";
      setPmBackgroundTaskStatus(resumeStatus);
      setPmPanelTaskId(newTaskId);
      setPmPanelTaskStatus(resumeStatus);
      pmBackgroundTaskAbortRef.current = streamPmResearchTask(newTaskId, {
        onEvent: (event) => applyPmTaskEvent(event),
        onAnswerDelta: (event) => {
          appendPmAnswerDelta(event.delta);
        },
        onImageContextWarning: handlePmImageContextWarning,
        onDone: (event) => {
          applyPmTaskEvent(event);
          flushVisibleStreamingText();
          const doneStatus = derivePmBackgroundTaskStatus(event);
          setPmBackgroundTaskStatus(doneStatus);
          setPmPanelTaskStatus(doneStatus);
          pmBackgroundTaskAbortRef.current = null;
          appendPmTerminalMessageIfNeeded(event);
        },
        onError: (err) => {
          setPmBackgroundTaskStatus("failed");
          setPmPanelTaskStatus("failed");
          resetStreamingText();
          setIsStreaming(false);
          onStreamingChange?.(false);
          message.error(`${t("chat.streamError")}: ${err}`);
        },
      });
      message.success(
        t("operations.pmBackgroundResumed", "已重新启动后台研究任务"),
      );
    } catch (err) {
      message.error(`${t("chat.streamError")}: ${(err as Error).message}`);
    }
  }, [
    appendPmAnswerDelta,
    appendPmTerminalMessageIfNeeded,
    applyPmTaskEvent,
    flushVisibleStreamingText,
    handlePmImageContextWarning,
    onStreamingChange,
    pmBackgroundTaskId,
    resetStreamingText,
    t,
  ]);

  useEffect(() => {
    if (sessionSource !== "pm") return;
    if (!isPmResearchTaskId(pmBackgroundTaskId)) return;
    const status = (pmBackgroundTaskStatus ?? "").toLowerCase();
    const shouldStream =
      status === "queued" || status === "running" || status === "cancelling";
    if (!shouldStream) return;
    if (pmBackgroundTaskAbortRef.current) return;

    pmBackgroundTaskAbortRef.current = streamPmResearchTask(
      pmBackgroundTaskId,
      {
        onEvent: (event) => applyPmTaskEvent(event),
        onAnswerDelta: (event) => {
          appendPmAnswerDelta(event.delta);
        },
        onImageContextWarning: handlePmImageContextWarning,
        onDone: (event) => {
          applyPmTaskEvent(event);
          flushVisibleStreamingText();
          const doneStatus = derivePmBackgroundTaskStatus(event);
          setPmBackgroundTaskStatus(doneStatus);
          setPmPanelTaskStatus(doneStatus);
          pmBackgroundTaskAbortRef.current = null;
          pmQueueStartingRef.current = false;
          appendPmTerminalMessageIfNeeded(event);
        },
        onError: (err) => {
          setPmBackgroundTaskStatus("failed");
          setPmPanelTaskStatus("failed");
          pmBackgroundTaskAbortRef.current = null;
          pmQueueStartingRef.current = false;
          resetStreamingText();
          setIsStreaming(false);
          onStreamingChange?.(false);
          message.error(`${t("chat.streamError")}: ${err}`);
        },
      },
    );
  }, [
    appendPmAnswerDelta,
    appendPmTerminalMessageIfNeeded,
    applyPmTaskEvent,
    flushVisibleStreamingText,
    handlePmImageContextWarning,
    onStreamingChange,
    pmBackgroundTaskId,
    pmBackgroundTaskStatus,
    resetStreamingText,
    sessionSource,
    t,
  ]);

  useEffect(() => {
    if (sessionSource !== "pm") return;
    if (!activeSessionId) return;
    if (pmQueueStartingRef.current) return;
    if (pmBackgroundTaskAbortRef.current) return;
    const currentBusy =
      pmBackgroundTaskStatus === "queued" ||
      pmBackgroundTaskStatus === "running" ||
      pmBackgroundTaskStatus === "cancelling";
    if (currentBusy) return;
    const next = pmPromptQueueRef.current[0];
    if (!next) return;
    const rest = pmPromptQueueRef.current.slice(1);
    commitPmPromptQueue(rest);
    void startPmQueuedPrompt(next, activeSessionId).catch(() => {
      // startPmQueuedPrompt already surfaced the error to the user.
    });
  }, [
    activeSessionId,
    pmBackgroundTaskStatus,
    pmPromptQueue,
    sessionSource,
    commitPmPromptQueue,
    startPmQueuedPrompt,
  ]);

  // ── Reply ──────────────────────────────────────────────────────────────────────────────────────
  const replyReference = useMemo(() => {
    if (!replyingTo) return null;
    const repliedMsg = displayMessages.find((m) => m.id === replyingTo);
    if (!repliedMsg) return null;
    return (
      contentToPlain(repliedMsg.content).slice(0, 120) || "[file or media]"
    );
  }, [replyingTo, displayMessages]);

  const replyPreviewByMessageId = useMemo(() => {
    const previews = new Map<string, string>();
    for (const msg of displayMessages) {
      if (!msg.replyTo) continue;
      const repliedMsg = displayMessages.find(
        (candidate) => candidate.id === msg.replyTo,
      );
      if (!repliedMsg) continue;
      const preview =
        contentToPlain(repliedMsg.content).slice(0, 160) || "[file or media]";
      previews.set(msg.id, preview);
    }
    return previews;
  }, [displayMessages]);

  const pmStageView = useMemo(() => {
    const stageLabelMap: Record<string, string> = {
      preflight: t("operations.pmStagePreflight", "启动预检"),
      resume: t("operations.pmStageResume", "恢复执行"),
      understand: t("operations.pmStageUnderstand", "任务理解"),
      report_extract: t("operations.pmStageReportExtract", "报告提取"),
      task_plan: t("operations.pmStageTaskPlan", "任务规划"),
      planner: t("operations.pmStagePlanner", "检索编排"),
      retrieve: t("operations.pmStageRetrieve", "多源检索"),
      deep_loop: t("operations.pmStageDeepLoop", "深度循环"),
      verify: t("operations.pmStageVerify", "证据校验"),
      retry_repair: t("operations.pmStageRetryRepair", "自动修复"),
      synthesize: t("operations.pmStageSynthesize", "总结输出"),
    };
    const verifyState = pmStageStates.verify;
    const synthesizeState = pmStageStates.synthesize;
    const verifyDetail = (verifyState?.detail ?? {}) as Record<string, unknown>;
    const retryRepairNotNeeded =
      !pmStageStates.retry_repair &&
      (synthesizeState?.status === "completed" ||
        synthesizeState?.status === "failed" ||
        (verifyState?.status === "completed" &&
          (verifyDetail.passed === true ||
            verifyDetail.qualityGateSkipped === true)));
    return PM_STAGE_ORDER.filter(
      (stage) =>
        !(
          (stage === "report_extract" || stage === "deep_loop") &&
          !pmStageStates[stage]
        ),
    ).map((stage) => {
      if (stage === "retry_repair" && retryRepairNotNeeded) {
        return {
          id: stage,
          label: stageLabelMap[stage] ?? stage,
          status: "completed" as const,
          attempt: verifyState?.attempt ?? 1,
          durationMs: null,
          detail: t("operations.pmRetryNotNeeded", "无需修复"),
          rawDetail: undefined,
          toolSummary: null,
          searchUsage: null,
        };
      }
      const state = pmStageStates[stage];
      const rawDetail =
        state?.detail &&
        typeof state.detail === "object" &&
        !Array.isArray(state.detail)
          ? (state.detail as Record<string, unknown>)
          : undefined;
      const eventToolSummary = (() => {
        if (stage !== "retrieve") return null;
        const list = pmStageEvents
          .filter((evt) => evt.stage === "retrieve")
          .map((evt) => {
            const d =
              evt.detail &&
              typeof evt.detail === "object" &&
              !Array.isArray(evt.detail)
                ? (evt.detail as Record<string, unknown>)
                : undefined;
            return parsePmToolSummary(d);
          })
          .filter((x): x is PmToolSummary => x != null);
        if (list.length === 0) return null;
        return list.reduce<PmToolSummary | null>(
          (acc, cur) => mergePmToolSummaries(acc, cur),
          null,
        );
      })();
      const toolSummary = mergePmToolSummaries(
        parsePmToolSummary(rawDetail),
        eventToolSummary,
      );
      const eventSearchUsage = (() => {
        if (stage !== "retrieve") return null;
        const list = pmStageEvents
          .filter((evt) => evt.stage === "retrieve")
          .map((evt) => {
            const d =
              evt.detail &&
              typeof evt.detail === "object" &&
              !Array.isArray(evt.detail)
                ? (evt.detail as Record<string, unknown>)
                : undefined;
            return parsePmSearchUsageSummary(d, parsePmToolSummary(d));
          })
          .filter((x): x is PmSearchUsageSummary => x != null);
        if (list.length === 0) return null;
        return list.reduce<PmSearchUsageSummary | null>(
          (acc, cur) => mergePmSearchUsageSummaries(acc, cur),
          null,
        );
      })();
      const searchUsage = mergePmSearchUsageSummaries(
        parsePmSearchUsageSummary(rawDetail, toolSummary),
        eventSearchUsage,
      );
      return {
        id: stage,
        label: stageLabelMap[stage] ?? stage,
        status: state?.status ?? "pending",
        attempt: state?.attempt ?? 1,
        durationMs: extractDurationMs(state?.detail),
        detail: toReadableStageDetail(
          stage,
          state?.detail,
          {
            nowMs: pmRuntimeTick,
            runningSinceMs: state?.runningSince,
            pipelineStartedAtMs: pmPipelineStartedAtRef.current ?? undefined,
          },
          state?.status,
        ),
        rawDetail,
        toolSummary,
        searchUsage,
      };
    });
  }, [pmRuntimeTick, pmStageStates, pmStageEvents, t]);

  const pmProgressPercent = useMemo(() => {
    const visibleStages = pmStageView.filter(
      (stage) => stage.id !== "deep_loop" || stage.status !== "pending",
    );
    const denominator = Math.max(visibleStages.length, 1);
    const terminalCount = visibleStages.filter((stage) =>
      isPmStageTerminal(stage.status),
    ).length;
    const hasRunning = visibleStages.some(
      (stage) => stage.status === "running",
    );
    const synthesizeDone = visibleStages.some(
      (stage) =>
        stage.id === "synthesize" &&
        isPmStageTerminal(stage.status),
    );
    if (!hasRunning && synthesizeDone) {
      return 100;
    }
    return Math.round((terminalCount / denominator) * 100);
  }, [pmStageView]);

  const pmStageEventView = useMemo(() => {
    const latestByStageAttempt = new Map<string, PmStageEvent>();
    for (const event of pmStageEvents) {
      latestByStageAttempt.set(`${event.stage}#${event.attempt}`, event);
    }
    return Array.from(latestByStageAttempt.values())
      .sort((a, b) => b.at - a.at)
      .slice(0, 8)
      .map((event) => {
        const rawDetail =
          event.detail &&
          typeof event.detail === "object" &&
          !Array.isArray(event.detail)
            ? (event.detail as Record<string, unknown>)
            : undefined;
        return {
          key: `${event.stage}-${event.status}-${event.at}`,
          label: stageLabelForNarrative(event.stage),
          status: event.status,
          attempt: event.attempt,
          durationMs: extractDurationMs(event.detail),
          detail: toReadableStageDetail(
            event.stage,
            event.detail,
            {
              nowMs: pmRuntimeTick,
              runningSinceMs: event.status === "running" ? event.at : undefined,
              pipelineStartedAtMs: pmPipelineStartedAtRef.current ?? undefined,
            },
            event.status,
          ),
          rawDetail,
        };
      });
  }, [pmRuntimeTick, pmStageEvents, stageLabelForNarrative]);

  const pmClaimAlignmentRows = useMemo(() => {
    return (pmQualitySnapshot?.claim_alignment ?? []).slice(0, 6);
  }, [pmQualitySnapshot]);

  const pmEvidenceTreeNodes = useMemo(() => {
    const fromBackend = (pmQualitySnapshot?.evidence_tree ?? []).slice(0, 8);
    if (fromBackend.length > 0) return fromBackend;
    return (pmQualitySnapshot?.claim_alignment ?? [])
      .slice(0, 8)
      .map((row) => ({
        claim: row.claim,
        status: row.cited ? "confirmed" : "gap",
        evidence_count: row.urls.length,
        evidences:
          row.urls.length > 0
            ? row.urls.slice(0, 4).map((url) => ({
                url,
                domain: extractUrlDomain(url) ?? "",
                excerpt: row.evidence_excerpt ?? row.claim,
              }))
            : [
                {
                  url: "",
                  domain: "",
                  excerpt: row.evidence_excerpt ?? row.claim,
                },
              ],
      }));
  }, [pmQualitySnapshot]);

  const pmConflictRows = useMemo(() => {
    return (pmQualitySnapshot?.conflict_matrix ?? []).slice(0, 6);
  }, [pmQualitySnapshot]);

  const pmConflictGraphSummary = useMemo(() => {
    return pmQualitySnapshot?.conflict_graph ?? null;
  }, [pmQualitySnapshot]);

  const pmStrategyLeaderboard = useMemo(
    () =>
      [...pmStrategyLeaderboardRows]
        .sort((a, b) => b.score - a.score || b.latestAt - a.latestAt)
        .slice(0, 6),
    [pmStrategyLeaderboardRows],
  );

  const pmPreferredStrategy = useMemo(() => {
    const candidate = pmStrategyLeaderboard[0];
    if (!candidate) return null;
    if (candidate.runs < 2) return null;
    return candidate;
  }, [pmStrategyLeaderboard]);

  const pmInlineNarrativeView = useMemo(() => {
    return pmInlineSegments.slice(-36);
  }, [pmInlineSegments]);

  const pmAutoSelectedStageId = useMemo(() => {
    const running = pmStageView.find((stage) => stage.status === "running");
    if (running) return running.id;
    const latestInline = [...pmInlineNarrativeView]
      .reverse()
      .find((seg) => !!seg.stage);
    if (latestInline?.stage) return latestInline.stage;
    const latestFinished = [...pmStageView]
      .reverse()
      .find(
        (stage) => stage.status === "completed" || stage.status === "failed",
      );
    if (latestFinished) return latestFinished.id;
    return pmStageView[0]?.id ?? null;
  }, [pmInlineNarrativeView, pmStageView]);

  useEffect(() => {
    if (!pmSelectedStageId) return;
    if (!pmStageView.some((stage) => stage.id === pmSelectedStageId)) {
      setPmSelectedStageId(null);
    }
  }, [pmSelectedStageId, pmStageView]);

  const pmEffectiveSelectedStageId = pmSelectedStageId ?? pmAutoSelectedStageId;

  const pmEffectiveSelectedStageLabel = useMemo(() => {
    if (!pmEffectiveSelectedStageId) return null;
    return (
      pmStageView.find((stage) => stage.id === pmEffectiveSelectedStageId)
        ?.label ?? null
    );
  }, [pmEffectiveSelectedStageId, pmStageView]);

  const pmInlineDetailsView = useMemo(() => {
    if (pmShowAllExecutionDetails || !pmEffectiveSelectedStageId) {
      return pmInlineNarrativeView;
    }
    return pmInlineNarrativeView.filter(
      (seg) => seg.stage === pmEffectiveSelectedStageId,
    );
  }, [
    pmEffectiveSelectedStageId,
    pmInlineNarrativeView,
    pmShowAllExecutionDetails,
  ]);

  const resolvePmInlineSegmentSummary = useCallback(
    (seg: PmInlineSegment): string => {
      const liveState = pmStageStates[seg.stage];
      if (liveState && liveState.attempt === seg.attempt) {
        return buildPmStageNarrative(
          seg.stage,
          liveState.status,
          liveState.detail as Record<string, unknown> | undefined,
          {
            nowMs: pmRuntimeTick,
            runningSinceMs: liveState.runningSince,
            pipelineStartedAtMs: pmPipelineStartedAtRef.current ?? undefined,
          },
        );
      }
      return seg.summary || fallbackStageNarrative(seg.stage, seg.status);
    },
    [buildPmStageNarrative, pmRuntimeTick, pmStageStates],
  );

  const pmCurrentNarrative = useMemo(() => {
    const target =
      [...pmInlineNarrativeView]
        .reverse()
        .find((seg) => seg.stage === pmEffectiveSelectedStageId) ??
      pmInlineNarrativeView[pmInlineNarrativeView.length - 1];
    if (!target) return "";
    const stageLabel = stageLabelForNarrative(target.stage);
    const summary = resolvePmInlineSegmentSummary(target).trim();
    if (!summary) return stageLabel;
    return `${stageLabel}: ${summary}`;
  }, [
    pmEffectiveSelectedStageId,
    pmInlineNarrativeView,
    resolvePmInlineSegmentSummary,
    stageLabelForNarrative,
  ]);

  const pmRecentFindings = useMemo(() => {
    const findings = [...pmInlineNarrativeView]
      .flatMap((seg) => seg.actions)
      .filter(
        (action) =>
          action.status === "success" &&
          action.detail &&
          action.detail.trim().length > 0,
      )
      .map((action) => action.detail!.trim());
    return findings.slice(-3);
  }, [pmInlineNarrativeView]);

  const pmInlineNarrativeEnabled =
    import.meta.env.VITE_PM_INLINE_NARRATIVE !== "0";

  const pmShouldShowInlineNarrative = useMemo(() => {
    if (!pmInlineNarrativeEnabled) return false;
    if (
      sessionSource !== "pm" ||
      pmSuppressExecutionUi ||
      (isStreaming && !superAssistantEndpoint)
    )
      return false;
    const stageStarted = PM_STAGE_ORDER.filter(
      (stage) => stage !== "preflight",
    ).some((stage) => {
      const status = pmStageStates[stage]?.status;
      return (
        status === "running" || status === "completed" || status === "failed"
      );
    });
    const hasNarrative = pmInlineNarrativeView.some(
      (seg) => seg.stage && seg.stage !== "preflight",
    );
    return stageStarted || hasNarrative;
  }, [
    isStreaming,
    pmInlineNarrativeEnabled,
    pmInlineNarrativeView,
    pmSuppressExecutionUi,
    pmStageStates,
    sessionSource,
    superAssistantEndpoint,
  ]);

  const pmNarrativeLeadText = useMemo(() => {
    const leadBlocks = [...pmInlineNarrativeView]
      .filter(
        (seg) =>
          (seg.stage === "understand" || seg.stage === "task_plan") &&
          seg.excerpt.trim().length > 0,
      )
      .slice(-4)
      .map((seg) => normalizePmNarrativeText(seg.excerpt, 1800))
      .filter(Boolean);
    const mergedLead = mergePmNarrativeBlocks(
      leadBlocks
        .flatMap((block) => block.split(/\n{2,}/))
        .map((block) => block.trim())
        .filter(Boolean),
    );
    if (mergedLead.length > 0) {
      return normalizePmNarrativeText(mergedLead, 3200);
    }
    const understandDetail =
      pmStageStates.understand?.detail &&
      typeof pmStageStates.understand.detail === "object" &&
      !Array.isArray(pmStageStates.understand.detail)
        ? (pmStageStates.understand.detail as Record<string, unknown>)
        : undefined;
    if (understandDetail) {
      const preview =
        pickPmDetailString(understandDetail, "humanSummary") ||
        pickPmDetailString(understandDetail, "prefaceText") ||
        pickPmDetailString(understandDetail, "preview") ||
        pickPmDetailString(understandDetail, "message");
      if (preview) {
        return normalizePmNarrativeText(
          sanitizePmUserFacingStageText(preview),
          1800,
        );
      }
    }
    return "";
  }, [pmInlineNarrativeView, pmStageStates]);

  const pmInlineStageTrail = useMemo(() => {
    const stageRows = pmStageView
      .filter((stage) => stage.id !== "preflight")
      .map((stage, index, arr) => ({
        id: stage.id,
        label: stage.label,
        status: stage.status,
        text:
          stage.status === "completed"
            ? t("operations.statusCompleted", "已完成")
            : stage.status === "failed"
              ? t("operations.statusFailed", "失败")
              : stage.status === "running"
                ? t("operations.statusRunning", "运行中")
                : t("common.pending", "待处理"),
        detail:
          typeof stage.detail === "string" && stage.detail.trim().length > 0
            ? shortHumanText(stage.detail, 140)
            : "",
        isTail: index === arr.length - 1,
      }));
    const hasStarted = stageRows.some((row) => row.status !== "pending");
    if (!hasStarted) return [];
    return stageRows;
  }, [pmStageView, t]);

  const pmInlineActionTrail = useMemo(() => {
    const actionsById = new Map<string, PmInlineAction & { stage: string }>();
    for (const segment of pmInlineSegments) {
      for (const action of segment.actions) {
        const candidate = { ...action, stage: segment.stage };
        const current = actionsById.get(action.id);
        if (!current || candidate.updatedAt >= current.updatedAt) {
          actionsById.set(action.id, candidate);
        }
      }
    }
    const mergedActions = Array.from(actionsById.values()).sort(
      (a, b) => a.createdAt - b.createdAt || a.index - b.index,
    );
    if (mergedActions.length === 0) return [];

    // Keep the trail monotonic during long-running tasks and avoid
    // "sudden shrink" when active stage switches.
    return mergedActions.slice(-48).map((action) => ({
      id: action.id,
      status: action.status,
      summary: shortHumanText(action.detail || action.name, 220),
      meta: [
        stageLabelForNarrative(action.stage),
        action.durationMs != null && action.durationMs > 0
          ? `${action.durationMs}ms`
          : "",
      ]
        .filter(Boolean)
        .join(" · "),
    }));
  }, [pmInlineSegments, stageLabelForNarrative]);

  const pmInlineNarrativeHasContent =
    pmNarrativeLeadText.trim().length > 0 ||
    pmInlineStageTrail.length > 0 ||
    pmInlineActionTrail.length > 0;

  const pmExecutionDrawerStages = useMemo(
    () =>
      pmStageView.map((stage) => ({
        id: stage.id,
        label: stage.label,
        status: stage.status,
        detail: stage.detail ?? "",
        toolSummary: stage.toolSummary ?? null,
        searchUsage: stage.searchUsage ?? null,
      })),
    [pmStageView],
  );

  const pmExecutionDrawerDetailRows = useMemo(
    () =>
      pmInlineDetailsView.map((seg) => {
        const segLiveState = pmStageStates[seg.stage];
        const displayStatus =
          segLiveState && segLiveState.attempt === seg.attempt
            ? segLiveState.status
            : seg.status;
        return {
          id: seg.id,
          label: stageLabelForNarrative(seg.stage),
          displayStatus,
          displaySummary: resolvePmInlineSegmentSummary(seg),
          actions: seg.actions,
          toolSummary: parsePmToolSummary(seg.rawDetail),
          searchUsage: parsePmSearchUsageSummary(
            seg.rawDetail,
            parsePmToolSummary(seg.rawDetail),
          ),
          excerpt: seg.excerpt,
        };
      }),
    [
      pmInlineDetailsView,
      pmStageStates,
      resolvePmInlineSegmentSummary,
      stageLabelForNarrative,
    ],
  );

  const pmPanelVisible = useMemo(() => {
    if (sessionSource !== "pm") return false;
    return (
      isStreaming ||
      pmBackgroundTaskId !== null ||
      Object.keys(pmStageStates).length > 0 ||
      pmStageEvents.length > 0 ||
      pmQualitySnapshot !== null
    );
  }, [
    isStreaming,
    pmBackgroundTaskId,
    pmQualitySnapshot,
    pmStageEvents,
    pmStageStates,
    sessionSource,
  ]);

  const pmBackgroundRunning =
    pmBackgroundTaskStatus === "queued" ||
    pmBackgroundTaskStatus === "running" ||
    pmBackgroundTaskStatus === "cancelling";

  const pmHasLiveExecution = useMemo(() => {
    if (sessionSource !== "pm") return false;
    return (
      isStreaming ||
      pmBackgroundRunning ||
      Object.keys(pmStageStates).length > 0 ||
      pmStageEvents.length > 0
    );
  }, [
    isStreaming,
    pmBackgroundRunning,
    pmStageEvents.length,
    pmStageStates,
    sessionSource,
  ]);

  const pmExecutionUiEnabled = useMemo(
    () => sessionSource === "pm" && !pmSuppressExecutionUi,
    [pmSuppressExecutionUi, sessionSource],
  );

  // A native-search failure is a terminal state for that stage. An older
  // runtime_wait heartbeat must not keep masking the failure while the server
  // is deciding whether to use a fallback provider/model.
  const pmRuntimeWaitIsStale = useMemo(() => {
    const runtimeWait = pmStageStates.runtime_wait;
    const nativeSearch = pmStageStates.native_web_search;
    return (
      runtimeWait?.status === "running" &&
      nativeSearch?.status === "failed" &&
      (nativeSearch.updatedAt ?? 0) >= (runtimeWait.updatedAt ?? 0)
    );
  }, [pmStageStates.native_web_search, pmStageStates.runtime_wait]);

  const pmStreamingPlaceholderText = useMemo(() => {
    if (!pmExecutionUiEnabled || !isStreaming) {
      return undefined;
    }
    const runningStage =
      pmStageStates.runtime_wait?.status === "running" &&
      !pmRuntimeWaitIsStale
        ? "runtime_wait"
        : pmStageStates.verification_repair?.status === "running"
          ? "verification_repair"
          : pmStageStates.native_web_search?.status === "running"
            ? "native_web_search"
            : pmStageStates.turn_model_started?.status === "running"
              ? "turn_model_started"
              : pmStageStates.retry_repair?.status === "running"
                ? "retry_repair"
                : pmStageStates.preflight?.status === "running"
                  ? "preflight"
                  : pmStageStates.report_extract?.status === "running"
                    ? "report_extract"
                    : pmStageStates.task_plan?.status === "running"
                      ? "task_plan"
                      : pmStageStates.understand?.status === "running"
                        ? "understand"
                        : pmStageStates.retrieve?.status === "running"
                          ? "retrieve"
                          : pmStageStates.verify?.status === "running"
                            ? "verify"
                            : pmStageStates.synthesize?.status === "running"
                              ? "synthesize"
                              : pmStageStates.planner?.status === "running"
                                ? "planner"
                                : null;
    if (runningStage === "runtime_wait") {
      const elapsed = pmStageStates.runtime_wait?.detail?.elapsedSeconds;
      return typeof elapsed === "number"
        ? `当前检索或校验仍在进行，已用时 ${elapsed} 秒...`
        : "当前检索或校验仍在进行...";
    }
    if (runningStage === "native_web_search") {
      return "正在联网检索最新信息并核对来源...";
    }
    if (runningStage === "verification_repair") {
      return "正在校验回答并补充缺失信息...";
    }
    if (runningStage === "turn_model_started") {
      return "正在思考并规划回答...";
    }
    if (runningStage === "retrieve") {
      return t(
        "operations.pmStageRetrieveRunning",
        "正在跨来源检索与抓取证据...",
      );
    }
    if (runningStage === "retry_repair") {
      return t("operations.pmStageRetryRunning", "正在自动修复证据缺口...");
    }
    if (runningStage === "preflight") {
      return t("operations.pmStagePreflightRunning", "正在执行启动健康检查...");
    }
    if (runningStage === "understand") {
      return t(
        "operations.pmStageUnderstandRunning",
        "正在理解你的问题与研究目标...",
      );
    }
    if (runningStage === "report_extract") {
      return t(
        "operations.pmStageReportExtractRunning",
        "正在提取报告里的指标、人群和约束...",
      );
    }
    if (runningStage === "task_plan") {
      return t(
        "operations.pmStageTaskPlanRunning",
        "正在生成任务规划与执行路径...",
      );
    }
    if (runningStage === "verify") {
      return t("operations.pmStageVerifyRunning", "正在校验证据与冲突...");
    }
    if (runningStage === "synthesize") {
      return t("operations.pmStageSynthesizeRunning", "正在汇总结论与建议...");
    }
    if (runningStage === "planner") {
      return t("chat.streamingPreparing", "正在思考并整理回答...");
    }
    return t(
      "operations.pmStageRunningGeneric",
      "正在思考并执行必要的检索与校验...",
    );
  }, [
    isStreaming,
    pmExecutionUiEnabled,
    pmRuntimeWaitIsStale,
    pmStageStates,
    t,
  ]);

  const pmLiveStageNotice = useMemo(() => {
    if (
      !pmExecutionUiEnabled ||
      !pmHasLiveExecution ||
      !shouldShowPmPostStreamNotice(
        streamCommittedRef.current,
        pmBackgroundRunning,
      )
    ) {
      return null;
    }
    if (isPmTaskTerminalStatus(pmBackgroundTaskStatus)) {
      return null;
    }
    const runningOrder: PmStageId[] = [
      ...(pmRuntimeWaitIsStale ? [] : ["runtime_wait" as PmStageId]),
      "verification_repair",
      "retry_repair",
      "understand",
      "report_extract",
      "task_plan",
      "planner",
      "native_web_search",
      "turn_model_started",
      "retrieve",
      "verify",
      "synthesize",
      "preflight",
    ];
    const runningStage = runningOrder.find(
      (stage) => pmStageStates[stage]?.status === "running",
    );
    const latestCompletedState = Object.values(pmStageStates)
      .filter((row) => row?.status === "completed" && !!row.stage)
      .sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0))[0];
    if (!runningStage) {
      if (pmBackgroundTaskStatus === "queued") {
        return {
          stage: "queued",
          text: t(
            "operations.pmTaskQueuedHint",
            "任务已排队，正在分配执行资源...",
          ),
        };
      }
      if (pmBackgroundRunning) {
        const latest = Object.values(pmStageStates)
          .filter((row) => !!row?.stage)
          .sort((a, b) => (b.updatedAt ?? 0) - (a.updatedAt ?? 0))[0];
        if (latest) {
          const detailText = toReadableStageDetail(
            latest.stage,
            latest.detail as Record<string, unknown> | undefined,
            {
              nowMs: pmRuntimeTick,
              runningSinceMs: latest.runningSince,
              pipelineStartedAtMs: pmPipelineStartedAtRef.current ?? undefined,
            },
            latest.status,
          );
          const label = stageLabelForNarrative(latest.stage);
          return {
            stage: latest.stage,
            text: detailText
              ? `${label}已完成，正在进入下一阶段 · ${detailText}`
              : `${label}已完成，正在进入下一阶段...`,
          };
        }
        return {
          stage: "pipeline",
          text: t(
            "operations.pmStageRunningGeneric",
            "正在思考并执行必要的检索与校验...",
          ),
        };
      }
      return null;
    }
    const state = pmStageStates[runningStage];
    const runningDetail =
      state?.detail && typeof state.detail === "object"
        ? (state.detail as Record<string, unknown>)
        : undefined;
    if (runningStage === "understand" && runningDetail) {
      const thinkingPreview = pickPmDetailString(runningDetail, "thinking");
      if (thinkingPreview) {
        return {
          stage: runningStage,
          text: `${t("chat.thinking", "思考中")} · ${shortHumanText(thinkingPreview, 160)}`,
        };
      }
    }
    const detailText = toReadableStageDetail(
      runningStage,
      state?.detail,
      {
        nowMs: pmRuntimeTick,
        runningSinceMs: state?.runningSince,
        pipelineStartedAtMs: pmPipelineStartedAtRef.current ?? undefined,
      },
      state?.status,
    );
    const label = stageLabelForNarrative(runningStage);
    const completedPrefix =
      latestCompletedState && latestCompletedState.stage !== runningStage
        ? `${stageLabelForNarrative(latestCompletedState.stage)}已完成，`
        : "";
    const text = detailText
      ? `${completedPrefix}${label}进行中 · ${detailText}`
      : `${completedPrefix}${label}进行中`;
    return { stage: runningStage, text };
  }, [
    pmBackgroundRunning,
    pmBackgroundTaskStatus,
    pmExecutionUiEnabled,
    pmHasLiveExecution,
    pmRuntimeTick,
    pmRuntimeWaitIsStale,
    pmStageStates,
    stageLabelForNarrative,
    t,
  ]);

  const pmLightweightWaitNotice = useMemo(() => {
    if (
      sessionSource !== "pm" ||
      isStreaming ||
      !pmSuppressExecutionUi ||
      !shouldShowPmPostStreamNotice(
        streamCommittedRef.current,
        pmBackgroundRunning,
      )
    ) {
      return null;
    }
    const status = (pmBackgroundTaskStatus ?? "").toLowerCase();
    if (status === "queued") {
      return t("chat.pmLightweightThinking", "正在思考并生成回复...");
    }
    if (status === "running" || status === "cancelling") {
      return t("chat.pmLightweightResponding", "正在整理回复，请稍候...");
    }
    return null;
  }, [
    isStreaming,
    pmBackgroundTaskStatus,
    pmBackgroundRunning,
    pmSuppressExecutionUi,
    sessionSource,
    t,
  ]);

  const latestAssistantUrls = useMemo(() => {
    const latestAssistant = [...displayMessages]
      .reverse()
      .find((m) => m.role === "assistant");
    if (!latestAssistant) return [];
    const text =
      typeof latestAssistant.content === "string"
        ? latestAssistant.content
        : contentToPlain(latestAssistant.content);
    return sanitizeEvidenceUrls(extractUrls(text), { limit: 8 });
  }, [displayMessages]);

  const latestAssistantMessage = useMemo(() => {
    return (
      [...displayMessages].reverse().find((m) => m.role === "assistant") ?? null
    );
  }, [displayMessages]);

  const latestAssistantPlainText = useMemo(() => {
    if (!latestAssistantMessage) return "";
    const persistedText = latestAssistantMessage.pmFinalDelivery?.response?.text;
    if (typeof persistedText === "string" && persistedText.trim()) {
      return persistedText;
    }
    return typeof latestAssistantMessage.content === "string"
      ? latestAssistantMessage.content
      : contentToPlain(latestAssistantMessage.content);
  }, [latestAssistantMessage]);

  const pmFinalDeliveryReady = useMemo(() => {
    return shouldShowPmFinalDelivery({
      sessionSource,
      executionUiEnabled: pmExecutionUiEnabled,
      suppressExecutionUi: pmSuppressExecutionUi,
      isStreaming,
      hasAssistantMessage: Boolean(latestAssistantMessage),
      synthStatus: pmStageStates.synthesize?.status,
      backgroundTaskStatus: pmBackgroundTaskStatus,
      latestTaskStatus: latestAssistantMessage?.pmTaskStatus,
      deliveryArtifact: latestAssistantMessage?.pmFinalDelivery,
      body: latestAssistantPlainText,
    });
  }, [
    isStreaming,
    latestAssistantMessage,
    latestAssistantPlainText,
    pmBackgroundTaskStatus,
    pmExecutionUiEnabled,
    pmStageStates,
    pmSuppressExecutionUi,
    sessionSource,
  ]);

  const pmFinalDeliveryTitle = useMemo(
    () =>
      extractPmDeliveryTitle(
        latestAssistantPlainText,
        t("operations.pmFinalDeliveryTitle", "研究交付总结"),
      ),
    [latestAssistantPlainText, t],
  );

  const pmFinalDeliveryHighlights = useMemo(
    () => extractPmDeliveryHighlights(latestAssistantPlainText, 4),
    [latestAssistantPlainText],
  );

  const pmPersistedDeliveryQuality = useMemo(
    () =>
      normalizePmQualitySnapshot(
        latestAssistantMessage?.pmFinalDelivery?.response?.pm_quality,
      ),
    [latestAssistantMessage?.pmFinalDelivery?.response?.pm_quality],
  );

  const pmFinalDeliverySources = useMemo(() => {
    const merged: string[] = [];
    merged.push(...(pmQualitySnapshot?.citations ?? []));
    merged.push(...(pmPersistedDeliveryQuality?.citations ?? []));
    merged.push(...extractAllUrls(latestAssistantPlainText));
    return sanitizeEvidenceUrls(merged, { dedupeByDomain: true, limit: 8 });
  }, [latestAssistantPlainText, pmPersistedDeliveryQuality?.citations, pmQualitySnapshot?.citations]);

  const pmQualityCitationUrls = useMemo(
    () =>
      sanitizeEvidenceUrls(pmQualitySnapshot?.citations ?? [], { limit: 10 }),
    [pmQualitySnapshot?.citations],
  );

  const pmFinalDeliverySourceLinks = useMemo(
    () =>
      pmFinalDeliverySources.map((url) => ({
        url,
        label: extractUrlDomain(url) ?? url,
      })),
    [pmFinalDeliverySources],
  );

  const pmFinalDeliveryPanel =
    pmFinalDeliveryReady &&
    (pmFinalDeliveryHighlights.length > 0 ||
      pmFinalDeliverySources.length > 0 ||
      latestAssistantPlainText.trim().length > 0) ? (
      <PmFinalDeliveryPanel
        t={t}
        title={pmFinalDeliveryTitle}
        highlights={pmFinalDeliveryHighlights}
        sources={pmFinalDeliverySourceLinks}
        body={latestAssistantPlainText}
      />
    ) : null;

  const pmSelectedClaimEvidence = useMemo(() => {
    if (pmSelectedClaimIndex == null) return null;
    const row = pmClaimAlignmentRows[pmSelectedClaimIndex];
    if (!row) return null;
    return {
      row,
      excerpt: extractClaimEvidenceExcerpt(latestAssistantPlainText, row.claim),
    };
  }, [latestAssistantPlainText, pmClaimAlignmentRows, pmSelectedClaimIndex]);

  useEffect(() => {
    if (pmClaimAlignmentRows.length === 0) {
      if (pmSelectedClaimIndex != null) {
        setPmSelectedClaimIndex(null);
      }
      return;
    }
    if (
      pmSelectedClaimIndex == null ||
      pmSelectedClaimIndex < 0 ||
      pmSelectedClaimIndex >= pmClaimAlignmentRows.length
    ) {
      setPmSelectedClaimIndex(0);
    }
  }, [pmClaimAlignmentRows, pmSelectedClaimIndex]);

  // Keep claim-evidence alignment as an advanced/debug-only surface.
  // Default UX follows Manus-like behavior: show clean narrative + citations,
  // avoid exposing internal quality-gate structures inline in the chat flow.
  const pmInlineEvidenceCardEnabled =
    import.meta.env.VITE_PM_INLINE_EVIDENCE_CARD === "1";

  const pmEvidenceInlinePanel =
    pmInlineEvidenceCardEnabled &&
    pmExecutionUiEnabled &&
    sessionSource === "pm" &&
    !isStreaming &&
    (pmQualitySnapshot?.citation_count ?? 0) > 0 &&
    (pmClaimAlignmentRows.length > 0 || pmConflictRows.length > 0) ? (
      <PmInlineEvidencePanel
        t={t}
        claimRows={pmClaimAlignmentRows}
        conflictRows={pmConflictRows}
        selectedClaimIndex={pmSelectedClaimIndex}
        selectedClaimEvidence={pmSelectedClaimEvidence}
        onSelectClaim={setPmSelectedClaimIndex}
      />
    ) : null;

  const pmInlineNarrativePanel =
    pmShouldShowInlineNarrative && pmInlineNarrativeHasContent ? (
      <PmInlineNarrativePanel
        t={t}
        leadText={pmNarrativeLeadText}
        actionTrail={pmInlineActionTrail}
        stageTrail={pmInlineStageTrail}
      />
    ) : null;

  const handleThinkingToggle = useCallback(
    () => setThinkingExpanded((value) => !value),
    [],
  );
  const handleReply = useCallback((messageId: string) => {
    setReplyingTo(messageId);
  }, []);
  const liveAdversarialRunId =
    activeAdversarialMeta.adversarialRunId ||
    (pmBackgroundTaskId && isChatAdversarialRunId(pmBackgroundTaskId)
      ? pmBackgroundTaskId
      : undefined);
  const liveAttributionTaskId =
    pmBackgroundTaskId && isNl2sqlAttributionTaskId(pmBackgroundTaskId)
      ? pmBackgroundTaskId
      : undefined;

  // ── Render ────────────────────────────────────────────────────────────────────────────────────
  return (
    <div style={{ display: "flex", height: "100%", overflow: "hidden" }}>
      {/* Left: Session list */}
      <div
        style={{
          width: sidebarCollapsed ? 0 : sidebarWidth,
          minWidth: sidebarCollapsed ? 0 : sidebarWidth,
          borderRight: sidebarCollapsed
            ? "none"
            : "1px solid var(--border-subtle)",
          background: "var(--bg-surface)",
          overflow: "hidden",
          display: "flex",
          flexDirection: "column",
          flexShrink: 0,
          height: "100%",
          transition: "width 220ms ease, min-width 220ms ease",
        }}
      >
        <div
          style={{
            width: sidebarWidth,
            height: "100%",
            opacity: sidebarCollapsed ? 0 : 1,
            pointerEvents: sidebarCollapsed ? "none" : "auto",
            transform: sidebarCollapsed
              ? `translateX(-${Math.max(200, sidebarWidth)}px)`
              : "translateX(0)",
            transition: "transform 220ms ease, opacity 180ms ease",
          }}
        >
          <SessionList
            sessions={sessions}
            activeSessionId={activeSessionId}
            onSelect={(id) => {
              const nextSession = sessions.find(
                (session) => session.sessionId === id,
              );
              if (nextSession) {
                setActiveMcpServers(nextSession.mcpServers ?? []);
                setActiveSkills(nextSession.skills ?? []);
              }
              setActiveSessionId(id);
              loadSessionMessages(id);
            }}
            onNew={handleNewSession}
            onDelete={handleDeleteSession}
            onRename={handleRenameSession}
            onTogglePin={handleTogglePin}
            onToggleBookmark={handleToggleBookmark}
            loading={sessionsLoading}
            emptyText={emptySessionText}
          />
        </div>
      </div>
      <div
        style={{
          width: 32,
          borderRight: "1px solid var(--border-subtle)",
          background: "var(--bg-surface)",
          flexShrink: 0,
          display: "flex",
          alignItems: "flex-start",
          justifyContent: "center",
          paddingTop: 8,
        }}
      >
        <Tooltip
          title={
            sidebarCollapsed
              ? t("chat.expandSessionList", "展开会话列表")
              : t("chat.collapseSessionList", "收起会话列表")
          }
        >
          <Button
            type="text"
            size="small"
            icon={
              sidebarCollapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />
            }
            onClick={() => setSidebarCollapsed((prev) => !prev)}
            aria-label={
              sidebarCollapsed
                ? t("chat.expandSessionList", "展开会话列表")
                : t("chat.collapseSessionList", "收起会话列表")
            }
            style={{
              width: 24,
              height: 24,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              color: "var(--text-secondary)",
            }}
          />
        </Tooltip>
      </div>

      {/* Center: Chat area */}
      <div
        style={{
          flex: 1,
          display: "flex",
          flexDirection: "column",
          background: "var(--bg-surface)",
          height: "100%",
          overflow: "hidden",
          minWidth: 0,
        }}
      >
        {/* Top bar */}
        <div
          style={{
            padding: "8px 16px",
            borderBottom: "1px solid var(--border-subtle)",
            display: "flex",
            alignItems: "center",
            gap: 12,
            flexShrink: 0,
            flexWrap: "wrap",
          }}
        >
          <Text style={{ fontSize: 12, color: "var(--text-secondary)" }}>
            {activeSessionId
              ? `${t("chat.sessionPrefix")}${activeSessionId.slice(0, 16)}...`
              : t("chat.newConversation")}
          </Text>
          {topBarExtra}
          {showConfigTags &&
            (effectiveMcpServers.length > 0 || effectiveSkills.length > 0) && (
              <Space size={4}>
                {effectiveMcpServers.map((srv) => (
                  <Tag key={srv} color="purple" style={{ fontSize: 11 }}>
                    MCP: {srv}
                  </Tag>
                ))}
                {effectiveSkills.map((skill) => (
                  <Tag key={skill} color="gold" style={{ fontSize: 11 }}>
                    Skill: {skill}
                  </Tag>
                ))}
              </Space>
            )}
          <Tooltip title={t("chat.downloadMarkdown", "下载本次对话 Markdown")}>
            <Button
              size="small"
              icon={<DownloadOutlined />}
              onClick={handleExportMarkdown}
              disabled={displayMessages.length === 0}
            >
              {t("chat.downloadMarkdownShort", "下载 Markdown")}
            </Button>
          </Tooltip>
          {topBarActions && (
            <div style={{ marginLeft: "auto" }}>{topBarActions}</div>
          )}
        </div>

        {/* Message-level Sources / Activity / Memory / Trace now live under each
            assistant reply, so history replay stays attached to the answer. */}

        {/* Message list */}
        <div
          {...messageListProps}
          ref={messageListRef}
          style={{
            flex: 1,
            overflow: "auto",
            padding: "16px 24px",
            display: "flex",
            flexDirection: "column",
            gap: 16,
            userSelect: "text",
            ...(messageListProps?.style ?? {}),
          }}
          onPointerDown={(event) => {
            const target = event.target as HTMLElement;
            if (!target.closest("button, a, input, textarea, [role='button']")) {
              messageTextSelectionActiveRef.current = true;
              autoFollowScrollRef.current = false;
            }
            messageListProps?.onPointerDown?.(event);
          }}
          onScroll={(e) => {
            const el = e.currentTarget;
            autoFollowScrollRef.current = isNearBottom(el);
            maybeLoadOlderFromScroll(el);
            messageListProps?.onScroll?.(e);
          }}
          onDrop={handleDrop}
          onDragOver={(e) => {
            e.preventDefault();
            setDraggingOver(true);
          }}
          onDragLeave={() => setDraggingOver(false)}
        >
          {displayMessages.length === 0 &&
            !isStreaming &&
            noSessionPlaceholder && (
              <div
                style={{
                  flex: 1,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                <div style={{ textAlign: "center" }}>
                  <div style={{ fontSize: 48, marginBottom: 16 }}>
                    {noSessionPlaceholder.emoji}
                  </div>
                  <Text
                    style={{
                      fontSize: 16,
                      color: "var(--text-secondary)",
                      display: "block",
                      marginBottom: 8,
                    }}
                  >
                    {noSessionPlaceholder.title}
                  </Text>
                  <Text type="secondary" style={{ fontSize: 13 }}>
                    {noSessionPlaceholder.description}
                  </Text>
                </div>
              </div>
            )}

          {activeSessionId && historyHasMore && displayMessages.length > 0 && (
            <div
              style={{
                display: "flex",
                justifyContent: "center",
                margin: "8px 0",
              }}
            >
              <Button
                size="small"
                loading={historyLoadingMore}
                onClick={() => {
                  void loadOlderSessionMessages();
                }}
              >
                {historyLoadingMore
                  ? t("chat.loadingOlderMessages", "正在加载更早消息...")
                  : t("chat.loadOlderMessages", "加载更早消息")}
              </Button>
            </div>
          )}

          {displayMessages.map((msg, msgIndex) => {
            const adversarialAuditPanel =
              msg.role === "assistant" && msg.adversarialRunId ? (
                <AdversarialAuditPanel runId={msg.adversarialRunId} />
              ) : undefined;
            const attributionAuditPanel =
              msg.role === "assistant" && msg.attributionTaskId ? (
                <AttributionAuditPanel taskId={msg.attributionTaskId} />
              ) : undefined;
            const nl2sqlAuditPanel =
              msg.role === "assistant" && hasNl2sqlAuditToolCalls(msg.toolCalls) ? (
                <Nl2sqlAuditPanel toolCalls={msg.toolCalls} />
              ) : undefined;
            const persistedDeliveryBody =
              msg.pmFinalDelivery?.response?.text?.trim() ||
              (msg.role === "assistant" ? contentToPlain(msg.content).trim() : "");
            const persistedDeliveryQuality = normalizePmQualitySnapshot(
              msg.pmFinalDelivery?.response?.pm_quality,
            );
            const persistedDeliveryUrls = sanitizeEvidenceUrls(
              [
                ...(persistedDeliveryQuality?.citations ?? []),
                ...extractAllUrls(persistedDeliveryBody),
              ],
              { dedupeByDomain: true, limit: 8 },
            );
            const persistedPmDeliveryPanel =
              msg.role === "assistant" &&
              msg.pmFinalDelivery?.deliveryStatus === "persisted" &&
              persistedDeliveryBody ? (
                <PmFinalDeliveryPanel
                  t={t}
                  title={extractPmDeliveryTitle(
                    persistedDeliveryBody,
                    t("operations.pmFinalDeliveryTitle", "研究交付总结"),
                  )}
                  highlights={extractPmDeliveryHighlights(
                    persistedDeliveryBody,
                    4,
                  )}
                  sources={persistedDeliveryUrls.map((url) => ({
                    url,
                    label: extractUrlDomain(url) ?? url,
                  }))}
                  body={persistedDeliveryBody}
                />
              ) : undefined;
            const attachPmTailPanel =
              persistedPmDeliveryPanel ??
              (msg.role === "assistant" &&
              latestAssistantMessage?.id === msg.id ? (
                pmFinalDeliveryPanel && pmEvidenceInlinePanel ? (
                  <div style={{ display: "grid", gap: 10 }}>
                    {pmFinalDeliveryPanel}
                    {pmEvidenceInlinePanel}
                  </div>
                ) : (
                  (pmFinalDeliveryPanel ?? pmEvidenceInlinePanel ?? undefined)
                )
              ) : undefined);
            const pmReplyExecutionAction =
              sessionSource === "pm" &&
              msg.role === "assistant" &&
              !!msg.pmTaskId ? (
                <Space size={2}>
                  {!!msg.pmTaskId && (
                    <Tooltip
                      title={t(
                        "operations.pmReplyExecutionPanelHint",
                        "查看本条回复的执行过程",
                      )}
                    >
                      <Button
                        type="text"
                        size="small"
                        icon={<ProfileOutlined />}
                        aria-label={t(
                          "operations.pmReplyExecutionPanelHint",
                          "查看本条回复的执行过程",
                        )}
                        onClick={() => openPmExecutionPanelForMessage(msg)}
                        style={{
                          color: "var(--text-muted)",
                          padding: "2px 6px",
                          height: 24,
                        }}
                      />
                    </Tooltip>
                  )}
                  <Tooltip
                    title={t(
                      "operations.pmReplySharePreviewHint",
                      "网页预览（可分享）",
                    )}
                  >
                    <Button
                      type="text"
                      size="small"
                      icon={<ShareAltOutlined />}
                      aria-label={t(
                        "operations.pmReplySharePreviewHint",
                        "网页预览（可分享）",
                      )}
                      onClick={() => openPmSharePreviewForMessage(msg)}
                      style={{
                        color: "var(--text-muted)",
                        padding: "2px 6px",
                        height: 24,
                      }}
                    />
                  </Tooltip>
                </Space>
              ) : undefined;
            const replyPreview = replyPreviewByMessageId.get(msg.id);
            const replyPreviewPanel = replyPreview ? (
              <div
                style={{
                  padding: "8px 10px",
                  borderLeft: "3px solid var(--accent-ai)",
                  borderRadius: 6,
                  background: "var(--bg-interactive)",
                  color: "var(--text-secondary)",
                  fontSize: 12,
                  whiteSpace: "pre-wrap",
                }}
              >
                <Text type="secondary" style={{ fontSize: 12 }}>
                  ↩ {t("chat.replyingTo")}:
                </Text>
                <div
                  style={{
                    marginTop: 4,
                    overflow: "hidden",
                    display: "-webkit-box",
                    WebkitLineClamp: 3,
                    WebkitBoxOrient: "vertical",
                  }}
                >
                  {replyPreview}
                </div>
              </div>
            ) : null;
            const messageExtraPanel =
              replyPreviewPanel ||
              attachPmTailPanel ||
              adversarialAuditPanel ||
              attributionAuditPanel ||
              nl2sqlAuditPanel ? (
                <>
                  {replyPreviewPanel}
                  {attachPmTailPanel}
                  {adversarialAuditPanel}
                  {attributionAuditPanel}
                  {nl2sqlAuditPanel}
                </>
              ) : undefined;
            const messageExtraActions = pmReplyExecutionAction;
            return (
              <div
                key={msg.id}
                id={`chat-msg-${msg.id}`}
                onMouseEnter={(e) => {
                  const actions =
                    e.currentTarget.querySelectorAll(".msg-actions");
                  actions.forEach(
                    (el) => ((el as HTMLElement).style.opacity = "1"),
                  );
                }}
                onMouseLeave={(e) => {
                  const actions =
                    e.currentTarget.querySelectorAll(".msg-actions");
                  actions.forEach(
                    (el) => ((el as HTMLElement).style.opacity = "0"),
                  );
                }}
              >
                <MessageBubble
                  message={msg as any}
                  modelName={
                    msg.localCommand
                      ? t("chat.localCommandModel", "AOS command")
                      : msg.modelName || visibleAssistantModelName
                  }
                  variant={
                    sessionSource === "pm"
                      ? "pm"
                      : sessionSource === "agent"
                        ? "agent"
                        : "chat"
                  }
                  traceEvents={msg.traceEvents}
                  extraPanel={messageExtraPanel}
                  extraActions={messageExtraActions}
                  thinkingExpanded={thinkingExpanded}
                  onThinkingToggle={handleThinkingToggle}
                  onReply={handleReply}
                />
              </div>
            );
          })}

          {approvalPaused && approvalPaused.approvals.length > 0 && (
            <Card
              size="small"
              title={t("chat.approvalRequired", "等待你的工具审批")}
              style={{ margin: "8px 0", borderColor: "var(--warning-color, #d89614)" }}
            >
              <Space direction="vertical" style={{ width: "100%" }} size={8}>
                {approvalPaused.approvals.map((approval) => (
                  <div key={approval.requestId} style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
                    <div style={{ flex: 1, minWidth: 220 }}>
                      <Typography.Text strong>{approval.toolName}</Typography.Text>
                      <div style={{ color: "var(--text-secondary)", fontSize: 12 }}>
                        {approval.reason || t("chat.approvalReasonDefault", "该工具需要更高权限才能执行")}
                      </div>
                      <div style={{ color: "var(--text-muted)", fontSize: 11 }}>
                        {approval.currentMode} → {approval.requiredMode}
                        {approval.expired ? ` · ${t("chat.approvalExpired", "已过期")}` : ""}
                      </div>
                    </div>
                    <Space size={6}>
                      <Button
                        size="small"
                        danger
                        loading={approvalResolvingId === approval.requestId}
                        disabled={approvalResolvingId !== null}
                        onClick={() => resolvePendingApproval(approval.requestId, approval.expired ? "deny" : "approve")}
                      >
                        {approval.expired ? t("chat.approvalContinue", "继续但不执行") : t("chat.approvalApprove", "批准")}
                      </Button>
                      {!approval.expired && (
                        <Button
                          size="small"
                          disabled={approvalResolvingId !== null}
                          onClick={() => resolvePendingApproval(approval.requestId, "deny")}
                        >
                          {t("chat.approvalDeny", "拒绝")}
                        </Button>
                      )}
                    </Space>
                  </div>
                ))}
              </Space>
            </Card>
          )}

          {pmLiveStageNotice && sessionSource === "pm" && !isStreaming && (
            <div style={{ marginTop: 6, marginBottom: 8 }}>
              <MessageBubble
                message={
                  {
                    id: "pm-live-stage-notice",
                    role: "assistant",
                    content: "",
                    timestamp: streamingMessageTimestamp ?? Date.now(),
                  } as any
                }
                isStreaming
                isStreamingBubble
                modelName={visibleAssistantModelName}
                streamingPlaceholderText={pmLiveStageNotice.text}
                variant="pm"
                extraPanel={pmInlineNarrativePanel ?? undefined}
              />
            </div>
          )}

          {pmLightweightWaitNotice &&
            sessionSource === "pm" &&
            !isStreaming && (
              <div style={{ marginTop: 6, marginBottom: 8 }}>
                <MessageBubble
                  message={
                    {
                      id: "pm-lightweight-wait-notice",
                      role: "assistant",
                      content: "",
                      timestamp: streamingMessageTimestamp ?? Date.now(),
                    } as any
                  }
                  isStreaming
                  isStreamingBubble
                  modelName={visibleAssistantModelName}
                  streamingPlaceholderText={pmLightweightWaitNotice}
                  variant="pm"
                />
              </div>
            )}

          {/* Streaming bubble */}
          {isStreaming && (
            <div>
              <MessageBubble
                message={
                  {
                    id: "streaming",
                    role: "assistant",
                    content: visibleStreamingText,
                    timestamp: streamingMessageTimestamp ?? Date.now(),
                    toolCalls: Object.values(toolCalls),
                    thinking: thinkingText,
                    thinkingLoading: thinkingLoading,
                    // Pass the live duration so the moment `thinking_end`
                    // (or the first text token) freezes it, the streaming
                    // bubble switches to "已深度思考 · Xs" without waiting
                    // for the full turn to complete.
                    thinkingDurationMs: thinkingDurationMs,
                    isStreaming: true,
                    judgeModel: activeAdversarialMeta.judgeModel,
                    winnerModel: activeAdversarialMeta.winnerModel,
                    winnerReason: activeAdversarialMeta.winnerReason,
                    adversarialRunId: activeAdversarialMeta.adversarialRunId,
                  } as any
                }
                isStreaming
                isStreamingBubble
                modelName={visibleAssistantModelName}
                variant={
                  sessionSource === "pm"
                    ? "pm"
                    : sessionSource === "agent"
                      ? "agent"
                      : "chat"
                }
                streamingPlaceholderText={pmStreamingPlaceholderText}
                extraPanel={
                  superAssistantEndpoint ? (
                    <>
                      {pmInlineNarrativePanel}
                      {liveAdversarialRunId ? (
                        <AdversarialAuditPanel
                          runId={liveAdversarialRunId}
                          live
                        />
                      ) : null}
                      {liveAttributionTaskId ? (
                        <AttributionAuditPanel
                          taskId={liveAttributionTaskId}
                          live
                        />
                      ) : null}
                      {hasNl2sqlAuditToolCalls(Object.values(toolCalls)) ||
                      Object.values(pmStageStates).some((state) =>
                        state.stage.startsWith("nl2sql_"),
                      ) ? (
                        <Nl2sqlAuditPanel
                          toolCalls={Object.values(toolCalls)}
                          progressEvents={nl2sqlProgressEventsFromStageEvents(pmStageEvents)}
                        />
                      ) : null}
                    </>
                  ) : undefined
                }
                thinkingExpanded={thinkingExpanded}
                onThinkingToggle={handleThinkingToggle}
              />
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>

        {/* Input area */}
        <div
          {...inputAreaProps}
          style={{
            padding: "12px 24px 16px",
            borderTop: "1px solid var(--border-subtle)",
            flexShrink: 0,
            position: "relative",
            ...(inputAreaProps?.style ?? {}),
          }}
          onDrop={handleDrop}
          onDragOver={(e) => {
            e.preventDefault();
            setDraggingOver(true);
          }}
          onDragLeave={() => setDraggingOver(false)}
        >
          {/* Drag overlay */}
          {draggingOver && (
            <div
              style={{
                position: "absolute",
                inset: 0,
                background: "rgba(124,58,237,0.08)",
                border: "2px dashed var(--accent-ai)",
                borderRadius: 12,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                zIndex: 10,
                pointerEvents: "none",
              }}
            >
              <Space direction="vertical" align="center">
                <span style={{ fontSize: 32 }}>📂</span>
                <Text style={{ color: "var(--accent-ai)", fontSize: 14 }}>
                  {t("chat.dropToUpload")}
                </Text>
              </Space>
            </div>
          )}

          {/* Reply reference */}
          {replyingTo && replyReference !== null && (
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "6px 10px",
                background: "var(--bg-interactive)",
                borderRadius: 8,
                marginBottom: 8,
                marginLeft: superAssistantEndpoint ? 52 : 0,
                maxWidth: superAssistantEndpoint
                  ? "calc(100% - 52px)"
                  : undefined,
                fontSize: 12,
              }}
            >
              <span style={{ color: "var(--text-muted)" }}>
                ↩ {t("chat.replyingTo")}:
              </span>
              <span
                style={{
                  color: "var(--text-secondary)",
                  flex: 1,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {replyReference}
              </span>
              <button
                onClick={() => setReplyingTo(null)}
                style={{
                  background: "none",
                  border: "none",
                  cursor: "pointer",
                  color: "var(--text-muted)",
                  marginLeft: "auto",
                }}
              >
                ✕
              </button>
            </div>
          )}

          {/* Attachments */}
          {attachments.length > 0 && (
            <div
              style={{
                display: "flex",
                flexWrap: "wrap",
                gap: 8,
                marginBottom: 8,
                marginLeft: superAssistantEndpoint ? 52 : 0,
                maxWidth: superAssistantEndpoint
                  ? "calc(100% - 52px)"
                  : undefined,
              }}
            >
              {attachments.map((att, i) => (
                <AttachmentChip
                  key={i}
                  block={att}
                  index={i}
                  fileRecord={
                    att.type === "document" && (att as DocumentBlock).fileId
                      ? chatFileRecords[(att as DocumentBlock).fileId!]
                      : undefined
                  }
                  onRemove={() =>
                    setAttachments((prev) => {
                      const next = [...prev];
                      const removed = next[i];
                      if (removed?.type === "image") {
                        const previewUrl = (removed as ImageBlock).previewUrl;
                        if (previewUrl && previewUrl.startsWith("blob:")) {
                          URL.revokeObjectURL(previewUrl);
                        }
                      }
                      next.splice(i, 1);
                      return next;
                    })
                  }
                />
              ))}
            </div>
          )}

          {(sessionSource === "chat" ||
            (memoryEnabled && showMemoryButton) ||
            inputToolbarExtra) && (
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                flexWrap: "wrap",
                marginBottom: 8,
                marginLeft: superAssistantEndpoint ? 52 : 0,
                maxWidth: superAssistantEndpoint
                  ? "calc(100% - 52px)"
                  : undefined,
                color: "var(--text-secondary)",
              }}
            >
              {sessionSource === "chat" && (
                <Tooltip
                  title={
                    webSearchAvailable
                      ? t(
                          "chat.webSearchToggleTooltip",
                          "Search On allows live search for this turn; Off blocks live search.",
                        )
                      : searchCapability?.missingReason ||
                        t(
                          "chat.webSearchUnavailable",
                          "Web search requires model-native search, a configured web search service, or MCP search/browser/fetch.",
                        )
                  }
                >
                  <span
                    style={{
                      display: "inline-flex",
                      alignItems: "center",
                      gap: 6,
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 10,
                      padding: "3px 7px",
                      background:
                        searchMode !== "off"
                          ? "rgba(24,144,255,0.08)"
                          : "var(--bg-elevated)",
                      opacity:
                        webSearchAvailable || searchMode !== "on" ? 1 : 0.62,
                    }}
                  >
                    <GlobalOutlined style={{ fontSize: 13 }} />
                    <Segmented
                      size="small"
                      value={searchMode}
                      disabled={isStreaming}
                      onChange={(value) => {
                        const next = value as "on" | "off";
                        if (next === "on" && !webSearchAvailable) {
                          message.info(
                            searchCapability?.missingReason ||
                              t(
                                "chat.webSearchUnavailable",
                                "Web search is unavailable.",
                              ),
                          );
                          setSearchMode("off");
                          return;
                        }
                        setSearchMode(next);
                      }}
                      options={[
                        { label: t("chat.searchOn", "On"), value: "on" },
                        { label: t("chat.searchOff", "Off"), value: "off" },
                      ]}
                    />
                  </span>
                </Tooltip>
              )}

              {memoryEnabled && showMemoryButton && (
                <Tooltip
                  title={t(
                    sessionSource === "pm"
                      ? "chat.pmMemoryAutoTooltip"
                      : "chat.memoryAutoTooltip",
                    sessionSource === "pm"
                      ? "PM session memory is on by default. You can review, pause, or delete memories from the Memory panel."
                      : "Memory is on by default. You can review, pause, or delete memories from the Memory panel.",
                  )}
                >
                  <button
                    type="button"
                    onClick={() => {
                      setMemoryDrawerOpen(true);
                      refreshChatMemories();
                    }}
                    style={{
                      display: "inline-flex",
                      alignItems: "center",
                      gap: 6,
                      border: "1px solid var(--border-subtle)",
                      borderRadius: 999,
                      padding: "4px 9px",
                      background: "var(--bg-elevated)",
                      color: "var(--text-secondary)",
                      cursor: "pointer",
                    }}
                  >
                    <ProfileOutlined style={{ fontSize: 13 }} />
                    <Text style={{ fontSize: 12 }}>
                      {memoryPaused
                        ? t("chat.memoryPaused", "Memory paused")
                        : t("chat.memoryAuto", "Memory")}
                    </Text>
                  </button>
                </Tooltip>
              )}

              {inputToolbarExtra}

              {attachedDocumentCount > 0 && (
                <span
                  style={{
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 6,
                    border: "1px solid var(--border-subtle)",
                    borderRadius: 999,
                    padding: "4px 9px",
                    background: "var(--bg-elevated)",
                    minWidth: 0,
                  }}
                >
                  <FileTextOutlined style={{ fontSize: 13 }} />
                  <Text style={{ fontSize: 12 }}>
                    {t("chat.fileContextActive", {
                      count: attachedDocumentCount,
                      defaultValue: "{{count}} file(s)",
                    })}
                  </Text>
                </span>
              )}
            </div>
          )}

          {shouldShowLegacyPmQueue(sessionSource, superAssistantEndpoint) && (
            <PmTaskQueuePanel
              t={t}
              queue={pmPromptQueue}
              backgroundRunning={pmBackgroundRunning}
              backgroundStatus={pmBackgroundTaskStatus}
              hasReplacementDraft={hasInput || attachments.length > 0}
              canReplaceOrRunHead={
                hasInput || attachments.length > 0 || pmPromptQueue.length > 0
              }
              onCancelCurrent={() => void cancelPmBackgroundResearch()}
              onReplaceCurrent={() => void replaceCurrentPmBackgroundResearch()}
              onMoveQueuedPrompt={movePmQueuedPrompt}
              onRemoveQueuedPrompt={removePmQueuedPrompt}
            />
          )}

          {/* Slash command panel */}
          {slashOpen && !isStreaming && (
            <SlashCommandPanel
              commands={allSlashCommands}
              filter={slashFilter}
              selectedIndex={slashSelected}
              onSelect={(cmd) => {
                setInput(`/${cmd.name} `);
                setSlashOpen(false);
                setSlashFilter("");
                setSlashSelected(0);
                composerRef.current?.focus();
              }}
              onHover={(i) => setSlashSelected(i)}
              onClose={() => {
                setSlashOpen(false);
                setSlashFilter("");
                setSlashSelected(0);
              }}
            />
          )}

          {/* Input row */}
          <div
            style={{
              display: "flex",
              gap: 8,
              alignItems: "center",
            }}
          >
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept="image/*,.txt,.md,.markdown,.csv,.json,.jsonl,.sql,.html,.htm,.css,.js,.ts,.tsx,.jsx,.xml,.log,.rtf,.docx,.xlsx"
              style={{ display: "none" }}
              onChange={async (e) => {
                if (e.target.files) await uploadFiles(e.target.files);
                if (fileInputRef.current) fileInputRef.current.value = "";
              }}
            />

            <Tooltip title={t("chat.attachFile")}>
              <Button
                aria-label={t("chat.attachFile")}
                icon={
                  uploading ? (
                    <Loading3QuartersOutlined spin />
                  ) : (
                    <PaperClipOutlined />
                  )
                }
                onClick={() => fileInputRef.current?.click()}
                disabled={uploading || isStreaming}
                style={{
                  height: 44,
                  width: 44,
                  borderRadius: 10,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  border: "1px solid var(--border-default)",
                  background: "var(--bg-elevated)",
                  color: "var(--text-secondary)",
                  flexShrink: 0,
                  alignSelf: "center",
                }}
              />
            </Tooltip>

            <IsolatedComposerTextarea
              ref={composerRef}
              disabled={isStreaming}
              onValueChange={handleInputChange}
              onPaste={handlePaste}
              onKeyDown={handleKeyDown}
              onCompositionStart={() => {
                isComposingRef.current = true;
              }}
              onCompositionEnd={(e) => {
                isComposingRef.current = false;
                const nextValue = e.currentTarget.value;
                syncInputValue(nextValue);
              }}
              placeholder={inputPlaceholder ?? t("chat.inputPlaceholder")}
              minHeight={CHAT_INPUT_MIN_HEIGHT_PX}
              maxHeight={CHAT_INPUT_MAX_HEIGHT_PX}
            />

            <Tooltip
              title={isStreaming ? t("chat.stopGenerating") : t("chat.send")}
            >
              <Button
                type={isStreaming ? "default" : "primary"}
                aria-label={
                  isStreaming ? t("chat.stopGenerating") : t("chat.send")
                }
                icon={
                  isStreaming ? (
                    <span
                      aria-hidden="true"
                      style={{
                        display: "block",
                        width: 11,
                        height: 11,
                        borderRadius: 2,
                        background: "currentColor",
                      }}
                    />
                  ) : (
                    <SendOutlined />
                  )
                }
                onClick={isStreaming ? handleStop : () => void handleSend()}
                disabled={!isStreaming && !hasInput && attachments.length === 0}
                style={{
                  height: 44,
                  width: 44,
                  borderRadius: isStreaming ? "50%" : 10,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  flexShrink: 0,
                  alignSelf: "center",
                  background: isStreaming ? "var(--bg-elevated)" : undefined,
                  borderColor: isStreaming
                    ? "var(--border-default)"
                    : undefined,
                  color: isStreaming ? "var(--text-primary)" : undefined,
                }}
              />
            </Tooltip>
          </div>

          {/* Hint bar */}
          {!isStreaming &&
            (inputHintBar ?? (
              <div
                style={{
                  marginTop: 6,
                  fontSize: 11,
                  color: "var(--text-muted)",
                  display: "flex",
                  gap: 12,
                }}
              >
                <span>⏎ send</span>
                <span>⇧⏎ new line</span>
                <span>/ slash commands</span>
                <span>📎 drag to attach</span>
              </div>
            ))}
        </div>
      </div>

      {pmExecutionUiEnabled &&
        pmPanelVisible &&
        (pmHasLiveExecution || pmPanelOpen || !!pmPanelTaskId) && (
          <PmExecutionDrawer
            t={t}
            open={pmPanelOpen}
            taskId={pmPanelTaskId}
            taskStatus={pmPanelTaskStatus}
            progressPercent={pmProgressPercent}
            currentNarrative={pmCurrentNarrative}
            recentFindings={pmRecentFindings}
            stages={pmExecutionDrawerStages}
            selectedStageId={
              pmShowAllExecutionDetails ? null : pmEffectiveSelectedStageId
            }
            selectedStageLabel={pmEffectiveSelectedStageLabel}
            showAllDetails={pmShowAllExecutionDetails}
            detailRows={pmExecutionDrawerDetailRows}
            isStreaming={isStreaming}
            onClose={() => setPmPanelOpen(false)}
            onSelectStage={(stageId) => {
              setPmShowAllExecutionDetails(false);
              setPmSelectedStageId(stageId);
            }}
            onToggleShowAllDetails={() =>
              setPmShowAllExecutionDetails((value) => !value)
            }
          />
        )}

      {/* Right panel (optional) */}
      {rightPanel && rightPanelOpen && (
        <div
          style={{
            width: 400,
            borderLeft: "1px solid var(--border-subtle)",
            background: "var(--bg-surface)",
            overflow: "hidden",
            display: "flex",
            flexDirection: "column",
            flexShrink: 0,
            height: "100%",
          }}
        >
          {rightPanel}
        </div>
      )}
      {memoryEnabled && (
        <Drawer
          title={t("chat.memoryPanelTitle", "Memory")}
          open={memoryDrawerOpen}
          onClose={() => {
            setMemoryDrawerOpen(false);
            setMemorySourceGroup("manual");
          }}
          width={420}
          destroyOnHidden={false}
        >
          <Space direction="vertical" size={14} style={{ width: "100%" }}>
            {activeSessionId && (
              <div
                style={{
                  border: "1px solid var(--border-subtle)",
                  borderRadius: 8,
                  padding: 12,
                  background: "var(--bg-elevated)",
                }}
              >
                <Space direction="vertical" size={8} style={{ width: "100%" }}>
                  <Space
                    align="center"
                    style={{ justifyContent: "space-between", width: "100%" }}
                  >
                    <Space size={6} wrap>
                      <Tag color="blue">
                        {t("chat.contextPanelTitle", "Context")}
                      </Tag>
                      {contextStatus?.unknownContextWindow && (
                        <Tag>
                          {t("chat.contextWindowEstimated", "estimated")}
                        </Tag>
                      )}
                      {contextStatus?.memoryState?.pollutionState ===
                        "polluted" && (
                        <Tag color="orange">
                          {t("chat.memoryPolluted", "external context")}
                        </Tag>
                      )}
                    </Space>
                    <Button
                      size="small"
                      onClick={() => void handleManualCompact()}
                      loading={contextCompacting}
                      disabled={
                        contextStatusFetching ||
                        !contextStatus ||
                        contextStatus.messageCount <= 4
                      }
                    >
                      {t("chat.compactNow", "Compact")}
                    </Button>
                  </Space>
                  {contextStatus ? (
                    <>
                      <div
                        style={{
                          height: 6,
                          borderRadius: 999,
                          background: "var(--bg-surface)",
                          overflow: "hidden",
                        }}
                      >
                        <div
                          style={{
                            width: `${Math.min(100, Math.max(0, contextStatus.contextUsagePercent))}%`,
                            height: "100%",
                            background:
                              contextStatus.contextUsagePercent >= 80
                                ? "var(--color-warning)"
                                : "var(--accent-ai)",
                          }}
                        />
                      </div>
                      <Space size={8} wrap>
                        <Tooltip
                          title={t(
                            "chat.contextUsageHint",
                            "Estimated active context used for compaction decisions. Provider billing is based on model-returned usage.",
                          )}
                        >
                          <Text type="secondary" style={{ fontSize: 12 }}>
                            {t(
                              "chat.contextUsage",
                              "Estimated context: {{used}} / {{limit}} tokens",
                              {
                                used: contextStatus.estimatedTokens.toLocaleString(),
                                limit:
                                  contextStatus.effectiveContextLimit.toLocaleString(),
                              },
                            )}
                          </Text>
                        </Tooltip>
                        <Text type="secondary" style={{ fontSize: 12 }}>
                          {t(
                            "chat.tokensUntilCompaction",
                            "{{tokenCount}} until compact",
                            {
                              tokenCount: Math.max(
                                0,
                                contextStatus.tokensUntilCompaction,
                              ).toLocaleString(),
                            },
                          )}
                        </Text>
                        {contextStatus.tokenEstimator && (
                          <Tag style={{ marginInlineEnd: 0 }}>
                            {t(
                              "chat.contextTokenEstimator",
                              "Estimator: {{estimator}}",
                              {
                                estimator: contextStatus.tokenEstimator,
                              },
                            )}
                          </Tag>
                        )}
                        {contextStatus.compactionCount > 0 && (
                          <Text type="secondary" style={{ fontSize: 12 }}>
                            {t("chat.compactionCount", "{{count}} compact(s)", {
                              count: contextStatus.compactionCount,
                            })}
                          </Text>
                        )}
                        {contextStatus.lastCompactionRemovedMessages > 0 && (
                          <Tag color="green" style={{ marginInlineEnd: 0 }}>
                            {t(
                              "chat.lastCompactRemoved",
                              "Last compact removed {{count}} messages",
                              {
                                count:
                                  contextStatus.lastCompactionRemovedMessages,
                              },
                            )}
                          </Tag>
                        )}
                      </Space>
                      {lastManualCompaction && (
                        <div
                          style={{
                            border: "1px dashed var(--border-subtle)",
                            borderRadius: 8,
                            padding: "8px 10px",
                            background: "var(--bg-surface)",
                          }}
                        >
                          <Space
                            direction="vertical"
                            size={4}
                            style={{ width: "100%" }}
                          >
                            <Space size={[6, 6]} wrap>
                              <Tag color="green" style={{ marginInlineEnd: 0 }}>
                                {t("chat.compactResult", "Context compacted")}
                              </Tag>
                              {lastManualCompaction?.strategy && (
                                <Tag style={{ marginInlineEnd: 0 }}>
                                  {lastManualCompaction.strategy}
                                </Tag>
                              )}
                              {lastManualCompaction && (
                                <>
                                  <Tag style={{ marginInlineEnd: 0 }}>
                                    {t(
                                      "chat.compactRemovedMessages",
                                      "removed {{count}} messages",
                                      {
                                        count:
                                          lastManualCompaction.removedMessageCount,
                                      },
                                    )}
                                  </Tag>
                                  <Tag style={{ marginInlineEnd: 0 }}>
                                    {t(
                                      "chat.compactSummaryTokens",
                                      "summary {{count}} tokens",
                                      {
                                        count:
                                          lastManualCompaction.summaryTokens,
                                      },
                                    )}
                                  </Tag>
                                  <Tag style={{ marginInlineEnd: 0 }}>
                                    {t(
                                      "chat.compactRetainedTail",
                                      "tail {{count}} tokens",
                                      {
                                        count:
                                          lastManualCompaction.retainedTailTokens,
                                      },
                                    )}
                                  </Tag>
                                </>
                              )}
                            </Space>
                          </Space>
                        </div>
                      )}
                      {memoryPollutionReasonLabel && (
                        <Text type="secondary" style={{ fontSize: 12 }}>
                          {t(
                            "chat.memoryPollutionReason",
                            "External context was used; this thread will not auto-save new long-term memories from it: {{reason}}",
                            {
                              reason: memoryPollutionReasonLabel,
                            },
                          )}
                        </Text>
                      )}
                    </>
                  ) : (
                    <Text type="secondary" style={{ fontSize: 12 }}>
                      {contextStatusFetching
                        ? t("chat.contextLoading", "Loading context status...")
                        : t(
                            "chat.contextUnavailable",
                            "Context status is unavailable.",
                          )}
                    </Text>
                  )}
                </Space>
              </div>
            )}
            <div
              style={{
                border: "1px solid var(--border-subtle)",
                borderRadius: 8,
                padding: 12,
                background: "var(--bg-elevated)",
              }}
            >
              <Space direction="vertical" size={8} style={{ width: "100%" }}>
                <Text style={{ color: "var(--text-secondary)", fontSize: 13 }}>
                  {t(
                    sessionSource === "pm"
                      ? "chat.pmMemoryPanelDescription"
                      : "chat.memoryPanelDescription",
                    sessionSource === "pm"
                      ? "PM session memory stores relevant project facts, preferences, and compacted session summaries for this PM workspace."
                      : "Memory is enabled by default and only injects relevant, non-sensitive preferences or facts.",
                  )}
                </Text>
                <Tooltip
                  title={
                    memoryPaused
                      ? t(
                          "chat.resumeMemoryTooltip",
                          "Resume using existing memories and allow new safe memories to be generated.",
                        )
                      : t(
                          "chat.pauseMemoryTooltip",
                          "Temporarily stop using and generating memories. Existing memories are kept and can be resumed later.",
                        )
                  }
                >
                  <Button
                    icon={
                      memoryPaused ? (
                        <PlayCircleOutlined />
                      ) : (
                        <PauseCircleOutlined />
                      )
                    }
                    onClick={() => void handleToggleMemoryPause()}
                    loading={memoryModeUpdating}
                  >
                    {memoryPaused
                      ? t("chat.resumeMemory", "Resume memory")
                      : t("chat.pauseMemory", "Pause memory")}
                  </Button>
                </Tooltip>
              </Space>
            </div>

            <Space.Compact style={{ width: "100%" }}>
              <Input
                value={memoryDraft}
                onChange={(e) => setMemoryDraft(e.target.value)}
                placeholder={t(
                  "chat.memoryInputPlaceholder",
                  "Remember a stable preference or project fact, e.g. always answer in concise Chinese",
                )}
                onPressEnter={() => void handleCreateMemory()}
              />
              <Tooltip
                title={t(
                  "chat.addMemoryTooltip",
                  "Save this stable preference or fact so future relevant turns can use it.",
                )}
              >
                <Button
                  type="primary"
                  icon={<PlusOutlined />}
                  onClick={() => void handleCreateMemory()}
                  loading={memoryCreating}
                  disabled={!memoryDraft.trim() || memoryCreating}
                >
                  {t("chat.addMemory", "Add")}
                </Button>
              </Tooltip>
            </Space.Compact>

            <Segmented
              size="small"
              value={memorySourceGroup}
              onChange={(value) =>
                setMemorySourceGroup(value as "manual" | "automatic")
              }
              options={[
                { label: t("chat.manualMemories", "Manual"), value: "manual" },
                {
                  label: t("chat.automaticMemories", "Automatic"),
                  value: "automatic",
                },
              ]}
            />

            <div
              style={{ maxHeight: 420, overflowY: "auto" }}
              onScroll={(event) => {
                if (memoryItemsFetchingNextPage || !memoryItemsHaveNextPage)
                  return;
                const target = event.currentTarget;
                const nearBottom =
                  target.scrollTop + target.clientHeight >=
                  target.scrollHeight - 48;
                if (nearBottom) void fetchNextMemoryItemsPage();
              }}
            >
              <List
                loading={memoryItemsLoading}
                dataSource={visibleMemoryItems}
                locale={{
                  emptyText: (
                    <Empty
                      image={Empty.PRESENTED_IMAGE_SIMPLE}
                      description={
                        memorySourceGroup === "manual"
                          ? t("chat.noManualMemories", "No manual memories yet")
                          : t(
                              "chat.noAutomaticMemories",
                              "No automatic memories yet",
                            )
                      }
                    />
                  ),
                }}
                footer={
                  memoryItemsFetchingNextPage ? (
                    <div
                      style={{
                        display: "flex",
                        justifyContent: "center",
                        padding: 8,
                      }}
                    >
                      <LoadingOutlined />
                    </div>
                  ) : null
                }
                renderItem={(item) => (
                  <List.Item
                    actions={[
                      <Popconfirm
                        key="delete"
                        title={t(
                          "chat.deleteMemoryConfirm",
                          "Delete this memory?",
                        )}
                        okText={t("common.delete", "Delete")}
                        cancelText={t("common.cancel", "Cancel")}
                        onConfirm={() => void handleDeleteMemory(item.id)}
                      >
                        <Button
                          size="small"
                          type="text"
                          danger
                          icon={<DeleteOutlined />}
                          loading={memoryDeletingId === item.id}
                          disabled={
                            !!memoryDeletingId && memoryDeletingId !== item.id
                          }
                        />
                      </Popconfirm>,
                    ]}
                  >
                    <List.Item.Meta
                      title={
                        <Space size={6} wrap>
                          <Tag
                            color={
                              item.source === "manual" ? "blue" : "geekblue"
                            }
                          >
                            {memoryTypeLabel(item.memoryType)}
                          </Tag>
                          {item.pinned && (
                            <Tag color="gold">{t("chat.pinned", "Pinned")}</Tag>
                          )}
                          {!item.enabled && (
                            <Tag>{t("chat.disabled", "Disabled")}</Tag>
                          )}
                        </Space>
                      }
                      description={
                        <Text style={{ color: "var(--text-primary)" }}>
                          {item.content}
                        </Text>
                      }
                    />
                  </List.Item>
                )}
              />
            </div>
          </Space>
        </Drawer>
      )}
    </div>
  );
}
