import { agentApi, apiKeysApi, streamChatAdversarialRunEvents } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { ChatAdversarialRun, ChatAdversarialStreamEvent } from '@/types';
import { Form, Input, Modal, message } from 'antd';
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AdversarialComposer } from './superAdversarial/AdversarialComposer';
import { DebateTimeline } from './superAdversarial/DebateTimeline';
import { RunHeader } from './superAdversarial/RunHeader';
import { ThreadSidebar } from './superAdversarial/ThreadSidebar';
import type { ThreadSummary, TimelineMessage } from './superAdversarial/types';
import { isSuperAdversarialNeedsModelsError } from '@/components/chat/superAssistantSlashCommands';
import {
  buildThreadTimeline,
  ADVERSARIAL_DEFAULT_MAX_ROUNDS,
  ADVERSARIAL_HARD_MAX_ROUNDS,
  getRunThreadId,
  getThreadDisplayTitle,
  isAntdFormValidationError,
  isChatModelKey,
  summarizeThreads,
} from './superAdversarial/utils';
import './SuperAdversarial.css';

const ADVERSARIAL_PAGE_SIZE = 20;
const ADVERSARIAL_THREAD_PAGE_SIZE = 3;
const ADVERSARIAL_TYPEWRITER_TICK_MS = 24;
const ADVERSARIAL_TYPEWRITER_MAX_CHARS_PER_TICK = 8;

export default function SuperAdversarial() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [activeRunId, setActiveRunId] = useState<string | null>(null);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [composeNewRun, setComposeNewRun] = useState(false);
  const [renameTarget, setRenameTarget] = useState<ThreadSummary | null>(null);
  const [renameTitle, setRenameTitle] = useState('');
  const [pinningThreadId, setPinningThreadId] = useState<string | null>(null);
  const [deletingThreadId, setDeletingThreadId] = useState<string | null>(null);
  const [renamingThreadId, setRenamingThreadId] = useState<string | null>(null);
  const stageRef = useRef<HTMLElement | null>(null);
  const lastAutoScrolledThreadRef = useRef<string | null>(null);
  const visibleMessageIdsRef = useRef<Set<string>>(new Set());
  const shouldStickToBottomRef = useRef(true);
  const lastTimelineSignatureRef = useRef('');
  const typewriterTimerRef = useRef<number | null>(null);
  const streamAbortRef = useRef<(() => void) | null>(null);
  const latestStreamSeqRef = useRef<Record<string, number>>({});
  const [liveAdversarialMessages, setLiveAdversarialMessages] = useState<Record<string, TimelineMessage>>({});
  const [form] = Form.useForm<{
    question: string;
    models: string[];
    max_rounds: number;
  }>();
  const draftQuestion = Form.useWatch('question', form);
  const draftModels = Form.useWatch('models', form);
  const draftMaxRounds = Form.useWatch('max_rounds', form);

  const apiKeysQ = useQuery({
    queryKey: queryKeys.apiKeys.list(),
    queryFn: () => apiKeysApi.list(),
  });

  const chatModelOptions = useMemo(() => {
    const seen = new Set<string>();
    return (apiKeysQ.data?.keys ?? [])
      .filter((key) => isChatModelKey(key) && key.runtime_available !== false)
      .sort((a, b) => (a.priority ?? 0) - (b.priority ?? 0))
      .flatMap((key) => {
        const model = key.model?.trim();
        const identity = model?.toLowerCase();
        if (!model || !identity || seen.has(identity)) return [];
        seen.add(identity);
        return [{ value: model, label: `${model} · ${key.provider}` }];
      });
  }, [apiKeysQ.data?.keys]);

  const adversarialRunsQ = useInfiniteQuery({
    queryKey: queryKeys.chatAdversarial.list(),
    initialPageParam: 1,
    queryFn: ({ pageParam }) =>
      agentApi.listChatAdversarialRuns({
        page: Number(pageParam) || 1,
        per_page: ADVERSARIAL_PAGE_SIZE,
      }),
    getNextPageParam: (lastPage, pages) => {
      if (lastPage.has_more) return (lastPage.page ?? pages.length) + 1;
      const loaded = pages.reduce((sum, page) => sum + (page.items?.length ?? 0), 0);
      const total = Number(lastPage.total ?? loaded);
      return loaded < total ? pages.length + 1 : undefined;
    },
    refetchInterval: 5000,
  });

  const adversarialRuns = useMemo(
    () => adversarialRunsQ.data?.pages.flatMap((page) => page.items ?? []) ?? [],
    [adversarialRunsQ.data?.pages]
  );
  const threadSummaries = useMemo(() => summarizeThreads(adversarialRuns), [adversarialRuns]);
  const adversarialTotal = adversarialRunsQ.data?.pages[0]?.total ?? adversarialRuns.length;

  useEffect(() => {
    if (!composeNewRun && !activeRunId && threadSummaries.length > 0) {
      setActiveRunId(threadSummaries[0].latest.id);
    }
  }, [activeRunId, threadSummaries, composeNewRun]);

  useEffect(() => {
    if (chatModelOptions.length === 0) return;
    const current = form.getFieldValue('models') as string[] | undefined;
    if (!current?.length) {
      form.setFieldsValue({
        models: chatModelOptions.slice(0, Math.min(2, chatModelOptions.length)).map((item) => item.value),
        max_rounds: ADVERSARIAL_DEFAULT_MAX_ROUNDS,
      });
    }
  }, [chatModelOptions, form]);

  const activeThreadQ = useInfiniteQuery({
    queryKey: activeRunId
      ? [...queryKeys.chatAdversarial.detail(activeRunId), 'thread', 'paged']
      : ['chatAdversarial', 'thread', 'empty'],
    initialPageParam: 1,
    queryFn: ({ pageParam }) =>
      agentApi.getChatAdversarialThread(activeRunId!, {
        page: Number(pageParam) || 1,
        per_page: ADVERSARIAL_THREAD_PAGE_SIZE,
      }),
    enabled: Boolean(activeRunId),
    getNextPageParam: (lastPage, pages) => {
      if (lastPage.has_more) return (lastPage.page ?? pages.length) + 1;
      const loaded = pages.reduce((sum, page) => sum + (page.items?.length ?? 0), 0);
      const total = Number(lastPage.total ?? loaded);
      return loaded < total ? pages.length + 1 : undefined;
    },
    refetchInterval: 5000,
  });

  const activeThreadRuns = useMemo(() => {
    const loaded = activeThreadQ.data?.pages.flatMap((page) => page.items ?? []) ?? [];
    if (loaded.length) {
      const map = new Map<string, ChatAdversarialRun>();
      for (const run of loaded) map.set(run.id, run);
      return Array.from(map.values()).sort((a, b) =>
        (a.iteration_no ?? 1) - (b.iteration_no ?? 1) ||
        a.created_at.localeCompare(b.created_at) ||
        a.id.localeCompare(b.id)
      );
    }
    const fallback = adversarialRuns.find((run) => run.id === activeRunId);
    return fallback ? [fallback] : [];
  }, [activeRunId, activeThreadQ.data?.pages, adversarialRuns]);

  const activeRun = activeThreadRuns[activeThreadRuns.length - 1] ?? null;
  const selectedThreadId = activeThreadQ.data?.pages[0]?.thread_id ?? (activeThreadRuns[0] ? getRunThreadId(activeThreadRuns[0]) : null);
  const followupParentRun = !composeNewRun && activeRun?.status === 'completed' ? activeRun : null;
  const persistedTimeline = useMemo(() => buildThreadTimeline(activeThreadRuns, t), [activeThreadRuns, t]);
  const timeline = useMemo(() => {
    const persistedIds = new Set(persistedTimeline.map((item) => item.id));
    const liveItems = Object.values(liveAdversarialMessages)
      .filter((item) => !persistedIds.has(item.id))
      .sort((a, b) => {
        const roundDiff = (a.round ?? 0) - (b.round ?? 0);
        if (roundDiff !== 0) return roundDiff;
        return a.id.localeCompare(b.id);
      });
    return [...persistedTimeline, ...liveItems];
  }, [liveAdversarialMessages, persistedTimeline]);
  const [animatedContent, setAnimatedContent] = useState<Record<string, string>>({});
  const animatedContentRef = useRef<Record<string, string>>({});
  const selectedModelCount = Array.isArray(draftModels) ? draftModels.length : 0;
  const canSubmitAdversarial =
    typeof draftQuestion === 'string' &&
    draftQuestion.trim().length > 0 &&
    selectedModelCount >= 2 &&
    selectedModelCount <= 3 &&
    typeof draftMaxRounds === 'number' &&
    draftMaxRounds >= 1 &&
    draftMaxRounds <= 50;

  const mergeLiveAdversarialEvent = (event: ChatAdversarialStreamEvent) => {
    if (!event.messageId || !event.runId) return;
    const isDelta = event.event.endsWith('_delta');
    const isStarted = event.event.endsWith('_started');
    const isCompleted = event.event.endsWith('_completed');
    const isFailed = event.event.endsWith('_failed');
    const isCancelled = event.event.endsWith('_cancelled');
    const isRenderable =
      event.event.startsWith('model_') ||
      event.event.startsWith('judge_') ||
      event.event.startsWith('final_');
    if (!isRenderable) {
      if (
        event.event === 'run_completed' ||
        event.event === 'run_failed' ||
        event.event === 'run_cancelled'
      ) {
        void qc.invalidateQueries({ queryKey: queryKeys.chatAdversarial.all });
        void qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
      }
      return;
    }

    setLiveAdversarialMessages((prev) => {
      const existing = prev[event.messageId];
      const role: TimelineMessage['role'] = event.event.startsWith('judge_')
        ? 'judge'
        : event.event.startsWith('final_')
          ? 'final'
          : 'model';
      const title =
        role === 'judge'
          ? t('chat.adversarialJudgeWithModel', {
              model: event.model || t('chat.adversarialUnknownModel'),
            })
          : role === 'final'
            ? t('chat.adversarialFinalWithModel', {
                model: event.model || t('chat.adversarialUnknownModel'),
              })
            : event.model || t('chat.adversarialUnknownModel');
      const subtitle =
        role === 'final'
          ? undefined
          : role === 'judge'
            ? t('chat.adversarialRoundJudge', { round: event.round || '?' })
            : t('chat.adversarialRoundSpeech', { round: event.round || '?' });
      const currentContent = existing?.content ?? '';
      const nextContent = isDelta
        ? `${currentContent}${event.delta ?? ''}`
        : event.text ?? currentContent;
      const next: TimelineMessage = {
        id: event.messageId,
        role,
        title,
        subtitle,
        content:
          nextContent ||
          (isStarted ? '' : event.error || t('chat.adversarialNoTrace')),
        model: event.model ?? existing?.model,
        round: event.round ?? existing?.round,
        error: Boolean(event.error) || isFailed,
        typing: (isStarted || isDelta) && !isCompleted && !isFailed && !isCancelled,
        animate: true,
      };
      if (isCancelled && !next.content.trim()) {
        next.content = t('chat.adversarialCancellingHint');
      }
      return { ...prev, [event.messageId]: next };
    });
  };

  const startAdversarialMut = useMutation({
    mutationFn: (values: {
      question: string;
      models: string[];
      max_rounds?: number;
      parent_run_id?: string;
    }) => agentApi.startChatAdversarialRun(values),
    onSuccess: async (run) => {
      message.success(t('chat.adversarialStarted'));
      setActiveRunId(run.id);
      setComposeNewRun(false);
      form.setFieldValue('question', '');
      await qc.invalidateQueries({ queryKey: queryKeys.chatAdversarial.all });
      await qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    },
    onError: (err) => {
      const error = (err as Error).message;
      if (isSuperAdversarialNeedsModelsError(error)) {
        message.warning(t('chat.adversarialNeedModels'));
      } else {
        message.error(`${t('chat.adversarialStartFailed')}: ${error}`);
      }
    },
  });

  const cancelRunMut = useMutation({
    mutationFn: (runId: string) => agentApi.cancelChatAdversarialRun(runId),
    onSuccess: async () => {
      message.success(t('chat.adversarialCancelRequested'));
      await qc.invalidateQueries({ queryKey: queryKeys.chatAdversarial.all });
      await qc.invalidateQueries({ queryKey: queryKeys.agentOps.all });
    },
    onError: (err) => message.error(`${t('chat.adversarialCancelFailed')}: ${(err as Error).message}`),
  });

  const updateThreadMut = useMutation({
    mutationFn: (values: {
      runId: string;
      threadId: string;
      title?: string;
      is_pinned?: boolean;
      kind: 'rename' | 'pin' | 'unpin';
    }) =>
      agentApi.updateChatAdversarialThread(values.runId, {
        title: values.title,
        is_pinned: values.is_pinned,
      }),
    onSuccess: async (_, values) => {
      if (values.kind === 'rename') {
        message.success(t('chat.adversarialRenameSuccess'));
        setRenameTarget(null);
        setRenameTitle('');
      } else if (values.kind === 'pin') {
        message.success(t('chat.adversarialPinSuccess'));
      } else {
        message.success(t('chat.adversarialUnpinSuccess'));
      }
      await qc.invalidateQueries({ queryKey: queryKeys.chatAdversarial.all });
    },
    onError: (err, values) => {
      const key =
        values.kind === 'rename'
          ? 'chat.adversarialRenameFailed'
          : values.kind === 'pin'
            ? 'chat.adversarialPinFailed'
            : 'chat.adversarialUnpinFailed';
      message.error(`${t(key)}: ${(err as Error).message}`);
    },
    onSettled: () => {
      setPinningThreadId(null);
      setRenamingThreadId(null);
    },
  });

  const deleteThreadMut = useMutation({
    mutationFn: (values: { runId: string; threadId: string; wasActive?: boolean }) =>
      agentApi.deleteChatAdversarialThread(values.runId),
    onMutate: (values) => {
      setDeletingThreadId(values.threadId);
    },
    onSuccess: async (_, values) => {
      message.success(t('chat.adversarialDeleteSuccess'));
      qc.removeQueries({ queryKey: queryKeys.chatAdversarial.detail(values.runId) });
      await qc.invalidateQueries({ queryKey: queryKeys.chatAdversarial.list() });
    },
    onError: (err, values) => {
      if (values.wasActive) {
        setActiveRunId(values.runId);
        setComposeNewRun(false);
      }
      message.error(`${t('chat.adversarialDeleteFailed')}: ${(err as Error).message}`);
    },
    onSettled: () => {
      setDeletingThreadId(null);
    },
  });

  const applyRunDefaults = (run: ChatAdversarialRun) => {
    const availableModels = new Set(chatModelOptions.map((item) => item.value));
    const models = run.models.filter((model) => availableModels.has(model)).slice(0, 3);
    form.setFieldsValue({
      question: '',
      models:
        models.length >= 2
          ? models
          : chatModelOptions.slice(0, Math.min(2, chatModelOptions.length)).map((item) => item.value),
      max_rounds: Math.min(
        Math.max(run.max_rounds ?? ADVERSARIAL_DEFAULT_MAX_ROUNDS, 1),
        ADVERSARIAL_HARD_MAX_ROUNDS,
      ),
    });
  };

  const selectRun = (run: ChatAdversarialRun) => {
    setActiveRunId(run.id);
    setComposeNewRun(false);
    applyRunDefaults(run);
  };

  const startNewRun = () => {
    setActiveRunId(null);
    setComposeNewRun(true);
    form.setFieldsValue({
      question: '',
      models: chatModelOptions.slice(0, Math.min(2, chatModelOptions.length)).map((item) => item.value),
      max_rounds: ADVERSARIAL_DEFAULT_MAX_ROUNDS,
    });
  };

  const submitAdversarial = async () => {
    if (startAdversarialMut.isPending) return;
    let values: {
      question: string;
      models: string[];
      max_rounds: number;
    };
    try {
      values = await form.validateFields();
    } catch (err) {
      if (isAntdFormValidationError(err)) {
        const firstError = err.errorFields?.[0]?.errors?.[0];
        if (firstError) message.warning(firstError);
        return;
      }
      message.error(`${t('chat.adversarialStartFailed')}: ${(err as Error).message}`);
      return;
    }
    const question = values.question.trim();
    if (!question) {
      form.setFields([
        {
          name: 'question',
          errors: [t('chat.adversarialQuestionRequired')],
        },
      ]);
      message.warning(t('chat.adversarialQuestionRequired'));
      return;
    }
    shouldStickToBottomRef.current = true;
    startAdversarialMut.mutate({
      question,
      models: values.models,
      max_rounds: values.max_rounds,
      parent_run_id: followupParentRun?.id,
    });
    scrollStageToBottom(true);
  };

  const openRenameThread = (thread: ThreadSummary) => {
    setRenameTarget(thread);
    setRenameTitle(getThreadDisplayTitle(thread.latest));
  };

  const submitRenameThread = () => {
    if (!renameTarget) return;
    const title = renameTitle.trim();
    if (!title) {
      message.warning(t('chat.adversarialThreadTitleRequired'));
      return;
    }
    setRenamingThreadId(renameTarget.threadId);
    updateThreadMut.mutate({
      runId: renameTarget.latest.id,
      threadId: renameTarget.threadId,
      title,
      kind: 'rename',
    });
  };

  const togglePinThread = (thread: ThreadSummary) => {
    const nextPinned = !thread.latest.thread_pinned;
    setPinningThreadId(thread.threadId);
    updateThreadMut.mutate({
      runId: thread.latest.id,
      threadId: thread.threadId,
      is_pinned: nextPinned,
      kind: nextPinned ? 'pin' : 'unpin',
    });
  };

  const deleteThread = (thread: ThreadSummary) => {
    const wasActive =
      selectedThreadId === thread.threadId ||
      activeThreadRuns.some((run) => getRunThreadId(run) === thread.threadId);
    if (wasActive) {
      void qc.cancelQueries({ queryKey: queryKeys.chatAdversarial.detail(thread.latest.id) });
      qc.removeQueries({ queryKey: queryKeys.chatAdversarial.detail(thread.latest.id) });
      lastAutoScrolledThreadRef.current = null;
      setActiveRunId(null);
      setComposeNewRun(true);
    }
    deleteThreadMut.mutate({ runId: thread.latest.id, threadId: thread.threadId, wasActive });
  };

  const loadOlderThreadRuns = async () => {
    if (activeThreadQ.isFetchingNextPage || !activeThreadQ.hasNextPage) return;
    const element = stageRef.current;
    const previousHeight = element?.scrollHeight ?? 0;
    await activeThreadQ.fetchNextPage();
    requestAnimationFrame(() => {
      if (!element) return;
      element.scrollTop += Math.max(0, element.scrollHeight - previousHeight);
    });
  };

  const scrollStageToBottom = (smooth = false) => {
    requestAnimationFrame(() => {
      const element = stageRef.current;
      if (!element) return;
      element.scrollTo({
        top: element.scrollHeight,
        behavior: smooth ? 'smooth' : 'auto',
      });
    });
  };

  useEffect(() => {
    return () => {
      if (typewriterTimerRef.current != null) {
        window.clearTimeout(typewriterTimerRef.current);
        typewriterTimerRef.current = null;
      }
      streamAbortRef.current?.();
      streamAbortRef.current = null;
    };
  }, []);

  useEffect(() => {
    streamAbortRef.current?.();
    streamAbortRef.current = null;
    setLiveAdversarialMessages({});
    if (!activeRun || !['queued', 'running', 'cancelling'].includes(activeRun.status)) {
      return;
    }
    const afterSeq = latestStreamSeqRef.current[activeRun.id] ?? 0;
    streamAbortRef.current = streamChatAdversarialRunEvents(
      activeRun.id,
      {
        onEvent: (event) => {
          latestStreamSeqRef.current[event.runId] = Math.max(
            latestStreamSeqRef.current[event.runId] ?? 0,
            event.seq ?? 0,
          );
          mergeLiveAdversarialEvent(event);
        },
        onError: (error) => {
          if (import.meta.env.DEV) {
            console.warn('[SuperAdversarial] SSE failed, polling will continue:', error);
          }
        },
        onEnd: () => {
          void qc.invalidateQueries({ queryKey: queryKeys.chatAdversarial.all });
        },
      },
      { afterSeq },
    );
    return () => {
      streamAbortRef.current?.();
      streamAbortRef.current = null;
    };
  }, [activeRun?.id, activeRun?.status]);

  useEffect(() => {
    visibleMessageIdsRef.current = new Set(timeline.map((item) => item.id));
    const initial: Record<string, string> = {};
    for (const item of timeline) {
      initial[item.id] = item.content;
    }
    animatedContentRef.current = initial;
    setAnimatedContent(initial);
  }, [selectedThreadId]);

  useEffect(() => {
    if (typewriterTimerRef.current != null) {
      window.clearTimeout(typewriterTimerRef.current);
      typewriterTimerRef.current = null;
    }

    const knownIds = visibleMessageIdsRef.current;
    const nextContent = { ...animatedContentRef.current };
    let hasChanged = false;

    for (const item of timeline) {
      if (item.typing) {
        knownIds.add(item.id);
        if (nextContent[item.id] !== item.content) {
          nextContent[item.id] = item.content;
          hasChanged = true;
        }
        continue;
      }
      const isNew = !knownIds.has(item.id);
      knownIds.add(item.id);
      if (item.role === 'user' || !item.animate || !isNew) {
        if (nextContent[item.id] !== item.content) {
          nextContent[item.id] = item.content;
          hasChanged = true;
        }
        continue;
      }
      nextContent[item.id] = '';
      hasChanged = true;
    }

    for (const id of Object.keys(nextContent)) {
      if (!timeline.some((item) => item.id === id)) {
        delete nextContent[id];
        knownIds.delete(id);
        hasChanged = true;
      }
    }

    if (hasChanged) {
      animatedContentRef.current = nextContent;
      setAnimatedContent(nextContent);
    }

    const drain = () => {
      typewriterTimerRef.current = null;
      const current = { ...animatedContentRef.current };
      let progressed = false;
      let hasPending = false;

      for (const item of timeline) {
        if (item.role === 'user' || item.typing || !item.animate) continue;
        const visible = current[item.id] ?? '';
        if (visible.length >= item.content.length) continue;
        const remaining = item.content.length - visible.length;
        const step = Math.min(
          ADVERSARIAL_TYPEWRITER_MAX_CHARS_PER_TICK,
          Math.max(1, Math.ceil(remaining / 56)),
        );
        current[item.id] = item.content.slice(0, visible.length + step);
        progressed = true;
        if (current[item.id].length < item.content.length) {
          hasPending = true;
        }
      }

      if (progressed) {
        animatedContentRef.current = current;
        setAnimatedContent(current);
        requestAnimationFrame(() => {
          const element = stageRef.current;
          if (element && shouldStickToBottomRef.current) {
            element.scrollTop = element.scrollHeight;
          }
        });
      }

      if (hasPending) {
        typewriterTimerRef.current = window.setTimeout(
          drain,
          ADVERSARIAL_TYPEWRITER_TICK_MS,
        );
      }
    };

    if (timeline.some((item) => item.animate && !item.typing && item.role !== 'user' && (animatedContentRef.current[item.id] ?? '').length < item.content.length)) {
      typewriterTimerRef.current = window.setTimeout(
        drain,
        ADVERSARIAL_TYPEWRITER_TICK_MS,
      );
    }
  }, [timeline]);

  const animatedTimeline = useMemo(
    () =>
      timeline.map((item) => ({
        ...item,
        content: animatedContent[item.id] ?? item.content,
        typing: item.typing || Boolean(item.animate && item.role !== 'user' && (animatedContent[item.id] ?? item.content).length < item.content.length),
      })),
    [animatedContent, timeline],
  );

  useEffect(() => {
    if (!selectedThreadId || activeThreadQ.isLoading || activeThreadQ.isFetchingNextPage) return;
    if (lastAutoScrolledThreadRef.current === selectedThreadId) return;
    lastAutoScrolledThreadRef.current = selectedThreadId;
    shouldStickToBottomRef.current = true;
    scrollStageToBottom();
  }, [activeThreadQ.isFetchingNextPage, activeThreadQ.isLoading, selectedThreadId, timeline.length]);

  useEffect(() => {
    if (!activeRun?.id || composeNewRun) return;
    if (!['queued', 'running', 'cancelling'].includes(activeRun.status)) return;
    shouldStickToBottomRef.current = true;
    scrollStageToBottom();
  }, [activeRun?.id, activeRun?.status, composeNewRun]);

  useEffect(() => {
    const signature = timeline
      .map((item) => `${item.id}:${item.content.length}:${item.typing ? 1 : 0}`)
      .join('|');
    if (signature === lastTimelineSignatureRef.current) return;
    lastTimelineSignatureRef.current = signature;
    if (shouldStickToBottomRef.current) {
      scrollStageToBottom();
    }
  }, [timeline]);

  return (
    <div
      className="super-adversarial"
      style={{
        gridTemplateColumns: sidebarCollapsed ? '64px minmax(0, 1fr)' : '320px minmax(0, 1fr)',
      }}
    >
      <ThreadSidebar
        t={t}
        collapsed={sidebarCollapsed}
        threads={threadSummaries}
        selectedThreadId={selectedThreadId}
        composeNewRun={composeNewRun}
        total={adversarialTotal}
        loading={adversarialRunsQ.isLoading}
        fetchingNextPage={adversarialRunsQ.isFetchingNextPage}
        hasNextPage={Boolean(adversarialRunsQ.hasNextPage)}
        pinningThreadId={pinningThreadId}
        deletingThreadId={deletingThreadId}
        renamingThreadId={renamingThreadId}
        onCollapseChange={setSidebarCollapsed}
        onNewRun={startNewRun}
        onSelectRun={selectRun}
        onTogglePin={togglePinThread}
        onRename={openRenameThread}
        onDelete={deleteThread}
        onLoadMore={() => void adversarialRunsQ.fetchNextPage()}
      />

      <main className="super-adversarial__main">
        <RunHeader
          t={t}
          activeRun={activeRun}
          composeNewRun={composeNewRun}
          cancelling={cancelRunMut.isPending}
          onCancel={() => {
            if (activeRun) cancelRunMut.mutate(activeRun.id);
          }}
        />
        <section
          ref={stageRef}
          className="super-adversarial__stage"
          onScroll={(event) => {
            const target = event.currentTarget;
            const distanceToBottom = target.scrollHeight - target.scrollTop - target.clientHeight;
            shouldStickToBottomRef.current = distanceToBottom < 96;
            if (target.scrollTop <= 56) void loadOlderThreadRuns();
          }}
        >
          <DebateTimeline
            t={t}
            composeNewRun={composeNewRun}
            timeline={animatedTimeline}
            hasOlder={Boolean(activeThreadQ.hasNextPage)}
            loadingOlder={activeThreadQ.isFetchingNextPage}
            reachedOldest={activeThreadRuns.length > ADVERSARIAL_THREAD_PAGE_SIZE}
            onLoadOlder={() => void loadOlderThreadRuns()}
          />
        </section>
        <AdversarialComposer
          t={t}
          form={form}
          modelOptions={chatModelOptions}
          followupParentRun={followupParentRun}
          canSubmit={canSubmitAdversarial}
          submitting={startAdversarialMut.isPending}
          onSubmit={submitAdversarial}
          onNewRun={startNewRun}
          onTooManyModels={() => message.warning(t('chat.adversarialMaxModels'))}
        />
      </main>
      <Modal
        title={t('chat.adversarialRenameTitle')}
        open={Boolean(renameTarget)}
        okText={t('common.confirm')}
        cancelText={t('common.cancel')}
        confirmLoading={Boolean(renamingThreadId)}
        onOk={submitRenameThread}
        onCancel={() => {
          setRenameTarget(null);
          setRenameTitle('');
        }}
      >
        <Input
          autoFocus
          maxLength={80}
          showCount
          value={renameTitle}
          placeholder={t('chat.adversarialThreadTitlePlaceholder')}
          onChange={(event) => setRenameTitle(event.target.value)}
          onPressEnter={submitRenameThread}
        />
      </Modal>
    </div>
  );
}
