import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Alert,
  Button,
  Card,
  Col,
  Drawer,
  Form,
  Image,
  Input,
  Modal,
  Popconfirm,
  Row,
  Select,
  Space,
  Statistic,
  Table,
  Tag,
  Typography,
  Upload,
  message,
} from 'antd';
import { DeleteOutlined, DownloadOutlined, EditOutlined, EyeOutlined, FilePdfOutlined, FilePptOutlined, PlusOutlined, ReloadOutlined, ShareAltOutlined, UploadOutlined } from '@ant-design/icons';
import { useMutation, useQueries, useQuery, useQueryClient } from '@tanstack/react-query';

import { pmApi, uploadFile, type PmMaterialAssetType, type PmTaskImageInput } from '@/api';
import { Markdown } from '@/components/chat';

const { Text } = Typography;

type MaterialJob = {
  id: number;
  missionRunId?: number | null;
  threadId?: number | null;
  parentJobId?: number | null;
  iterationNo: number;
  promptText: string;
  model?: string | null;
  assetType: string;
  status: string;
  resultCount: number;
  errorMessage?: string | null;
  createdAt: string;
  updatedAt: string;
};

type MaterialThread = {
  threadId: number;
  latestJobId: number;
  missionRunId?: number | null;
  versionCount: number;
  latestIterationNo: number;
  promptText: string;
  model?: string | null;
  assetType: string;
  status: string;
  resultCount: number;
  errorMessage?: string | null;
  createdBy?: string | null;
  createdAt: string;
  updatedAt: string;
};

type MaterialAssetType = PmMaterialAssetType;
type MaterialWorkflowStage =
  | 'lyrics_draft'
  | 'composition_plan'
  | 'outline_draft'
  | 'slide_blueprint'
  | 'visual_plan'
  | 'generate';

type MaterialAsset = {
  id: number;
  jobId: number;
  assetType: string;
  url?: string | null;
  contentText?: string | null;
  meta: Record<string, unknown>;
  createdAt: string;
};

type ContinueContext = {
  threadId: number;
  parentJobId: number;
  continueFromAssetId: number;
  workflowStage?: MaterialWorkflowStage;
  promptText?: string;
};

const PM_SHARE_PREVIEW_MAX_CHARS = 18000;
const MATERIAL_CONTENT_BOX_STYLE: CSSProperties = {
  maxHeight: 260,
  overflowY: 'auto',
  overflowX: 'hidden',
  padding: '0 2px',
  width: '100%',
  minWidth: 0,
  wordBreak: 'break-word',
  overflowWrap: 'anywhere',
};
const MATERIAL_MEDIA_TEXT_BOX_STYLE: CSSProperties = {
  ...MATERIAL_CONTENT_BOX_STYLE,
  maxHeight: 160,
};

const MATERIAL_WORKFLOW_STAGE_MAP: Partial<Record<MaterialAssetType, MaterialWorkflowStage[]>> = {
  music: ['lyrics_draft', 'composition_plan', 'generate'],
  ppt: ['outline_draft', 'slide_blueprint', 'visual_plan', 'generate'],
};
const MATERIAL_CREATE_ASSET_TYPES: MaterialAssetType[] = ['text', 'image', 'music'];
const MATERIAL_FILTER_ASSET_TYPES: MaterialAssetType[] = ['text', 'image', 'music'];

function normalizeMaterialAssetType(assetType?: unknown): MaterialAssetType {
  const normalized = String(assetType ?? 'text').trim().toLowerCase();
  if (['text', 'image', 'music', 'ppt'].includes(normalized)) {
    return normalized as MaterialAssetType;
  }
  return 'text';
}

function workflowStagesForAssetType(assetType?: MaterialAssetType): MaterialWorkflowStage[] {
  if (!assetType) return [];
  return MATERIAL_WORKFLOW_STAGE_MAP[assetType] ?? [];
}

function nextWorkflowStageForAsset(
  assetType: MaterialAssetType,
  currentStage?: string,
): MaterialWorkflowStage | undefined {
  const stages = workflowStagesForAssetType(assetType);
  if (stages.length === 0) return undefined;
  if (!currentStage) return stages[0];
  const idx = stages.findIndex((stage) => stage === currentStage);
  if (idx < 0) return stages[0];
  return stages[Math.min(idx + 1, stages.length - 1)];
}

function extractWorkflowStageFromAsset(asset?: MaterialAsset): string | undefined {
  const raw = asset?.meta?.workflowStage;
  return typeof raw === 'string' && raw.trim().length > 0 ? raw.trim() : undefined;
}

function extractGeneratedKindFromAsset(asset?: MaterialAsset): string | undefined {
  const extra = asset?.meta?.extra;
  if (!extra || typeof extra !== 'object' || Array.isArray(extra)) return undefined;
  const raw = (extra as Record<string, unknown>).generatedKind;
  return typeof raw === 'string' && raw.trim().length > 0 ? raw.trim() : undefined;
}

function isPptFinalDeckAsset(asset?: MaterialAsset): boolean {
  if (!asset || asset.assetType.toLowerCase() !== 'ppt') return false;
  if (extractWorkflowStageFromAsset(asset) !== 'generate') return false;
  if (extractGeneratedKindFromAsset(asset) === 'html_ppt') return true;
  const normalizedUrl = normalizeAssetUrl(asset.url);
  const cleanPath = normalizedUrl?.split('?')[0]?.split('#')[0]?.toLowerCase() ?? '';
  const content = asset.contentText?.trim().toLowerCase() ?? '';
  return cleanPath.endsWith('.html') || content.startsWith('<!doctype html') || content.includes('<html');
}

type PmSharePreviewPayload = {
  schema: 'aos-pm-share-v1';
  title: string;
  generatedAt: string;
  messageId: string;
  taskId?: string | null;
  content: string;
  truncated?: boolean;
};

function encodeUtf8ToBase64Url(raw: string): string {
  const bytes = new TextEncoder().encode(raw);
  let binary = '';
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/g, '');
}

function buildPmSharePreviewUrl(payload: PmSharePreviewPayload): string {
  if (typeof window === 'undefined') return '';
  const encoded = encodeURIComponent(encodeUtf8ToBase64Url(JSON.stringify(payload)));
  const next = new URL(window.location.href);
  next.pathname = '/preview/share';
  next.search = `?d=${encoded}`;
  next.hash = '';
  return next.toString();
}

function buildMaterialAssetMarkdown(
  asset: MaterialAsset,
  labels: { typeLabel: string; contentLabel: string; urlLabel: string },
): string {
  const normalizedUrl = normalizeAssetUrl(asset.url);
  const contentBlocks: string[] = [];
  if (asset.contentText?.trim()) {
    contentBlocks.push(asset.contentText.trim());
  }
  if (asset.assetType?.toLowerCase() === 'image' && normalizedUrl) {
    if (contentBlocks.length > 0) contentBlocks.push('');
    contentBlocks.push(`![Material Asset #${asset.id}](${normalizedUrl})`);
  }
  if (asset.assetType?.toLowerCase() === 'music' && normalizedUrl) {
    if (contentBlocks.length > 0) contentBlocks.push('');
    contentBlocks.push(`[Audio: Material Asset #${asset.id}](${normalizedUrl})`);
  }
  const content = contentBlocks.length > 0 ? contentBlocks.join('\n') : '-';
  const lines: string[] = [
    `# Material Asset #${asset.id}`,
    '',
    `- ${labels.typeLabel}: ${asset.assetType || '-'}`,
    `- ${labels.urlLabel}: ${normalizedUrl || '-'}`,
    '',
    `## ${labels.contentLabel}`,
    '',
    content,
  ];
  return lines.join('\n');
}

function buildMaterialAssetThreadSectionMarkdown(
  asset: MaterialAsset,
  labels: {
    assetLabel: string;
    typeLabel: string;
    workflowStageLabel: string;
    urlLabel: string;
    contentLabel: string;
    createdAtLabel: string;
  },
  workflowStageText?: string,
): string {
  const normalizedUrl = normalizeAssetUrl(asset.url);
  const contentBlocks: string[] = [];
  if (asset.contentText?.trim()) {
    contentBlocks.push(asset.contentText.trim());
  }
  if (asset.assetType?.toLowerCase() === 'image' && normalizedUrl) {
    if (contentBlocks.length > 0) contentBlocks.push('');
    contentBlocks.push(`![Material Asset #${asset.id}](${normalizedUrl})`);
  }
  if (asset.assetType?.toLowerCase() === 'music' && normalizedUrl) {
    if (contentBlocks.length > 0) contentBlocks.push('');
    contentBlocks.push(`[Audio: Material Asset #${asset.id}](${normalizedUrl})`);
  }
  const content = contentBlocks.length > 0 ? contentBlocks.join('\n') : '-';
  const lines: string[] = [
    `#### ${labels.assetLabel} #${asset.id}`,
    '',
    `- ${labels.typeLabel}: ${asset.assetType || '-'}`,
    `- ${labels.workflowStageLabel}: ${workflowStageText || '-'}`,
    `- ${labels.urlLabel}: ${normalizedUrl || '-'}`,
    `- ${labels.createdAtLabel}: ${asset.createdAt || '-'}`,
    '',
    `##### ${labels.contentLabel}`,
    '',
    content,
  ];
  return lines.join('\n');
}

function normalizeAssetUrl(url?: string | null): string | null {
  const raw = url?.trim();
  if (!raw) return null;
  if (raw.startsWith('http://') || raw.startsWith('https://') || raw.startsWith('/')) return raw;
  return `/${raw}`;
}

function materialAssetExtension(asset: MaterialAsset, contentType?: string | null): string {
  const normalizedUrl = normalizeAssetUrl(asset.url);
  const path = normalizedUrl?.split('?')[0]?.split('#')[0] ?? '';
  const fromUrl = path.match(/\.([a-z0-9]{2,8})$/i)?.[1]?.toLowerCase();
  if (fromUrl) return fromUrl;
  const type = asset.assetType.toLowerCase();
  const mime = (contentType ?? '').toLowerCase();
  if (mime.includes('png')) return 'png';
  if (mime.includes('jpeg') || mime.includes('jpg')) return 'jpg';
  if (mime.includes('webp')) return 'webp';
  if (mime.includes('gif')) return 'gif';
  if (mime.includes('mpeg') || mime.includes('mp3')) return 'mp3';
  if (mime.includes('wav')) return 'wav';
  if (mime.includes('ogg')) return 'ogg';
  if (mime.includes('flac')) return 'flac';
  if (mime.includes('mp4')) return 'mp4';
  if (mime.includes('html')) return 'html';
  if (mime.includes('pdf')) return 'pdf';
  if (mime.includes('presentationml') || mime.includes('powerpoint')) return 'pptx';
  if (type === 'image') return 'png';
  if (type === 'music') return 'mp3';
  if (type === 'ppt') return 'html';
  return 'md';
}

function saveBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

function uploadAuthHeaders(): Record<string, string> {
  const token = localStorage.getItem('token');
  const tenantId = localStorage.getItem('tenant_id');
  const headers: Record<string, string> = {};
  if (token) headers.Authorization = `Bearer ${token}`;
  if (tenantId) {
    headers['X-Tenant-ID'] = tenantId;
    headers['X-Tenant-Id'] = tenantId;
  }
  return headers;
}

function revokeObjectUrl(url?: string): void {
  if (url && url.startsWith('blob:')) {
    URL.revokeObjectURL(url);
  }
}

function percentText(value?: number | null): string {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-';
  return `${Math.round(value * 1000) / 10}%`;
}

export default function OperationsMaterials() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const [jobOpen, setJobOpen] = useState(false);
  const [assetDrawerOpen, setAssetDrawerOpen] = useState(false);
  const [currentJob, setCurrentJob] = useState<MaterialJob | null>(null);
  const [currentThreadId, setCurrentThreadId] = useState<number | null>(null);
  const [continueContext, setContinueContext] = useState<ContinueContext | null>(null);
  const [jobPage, setJobPage] = useState(1);
  const [jobPageSize, setJobPageSize] = useState(20);
  const [jobAssetTypeFilter, setJobAssetTypeFilter] = useState<MaterialAssetType | undefined>(undefined);
  const [jobStatusFilter, setJobStatusFilter] = useState<string | undefined>(undefined);
  const [expandedThreadIds, setExpandedThreadIds] = useState<number[]>([]);
  const [deletingJobId, setDeletingJobId] = useState<number | null>(null);
  const [deletingThreadId, setDeletingThreadId] = useState<number | null>(null);
  const [viewingTextAsset, setViewingTextAsset] = useState<MaterialAsset | null>(null);
  const [jobForm] = Form.useForm();
  const [referenceImages, setReferenceImages] = useState<PmTaskImageInput[]>([]);
  const [referenceUploading, setReferenceUploading] = useState(false);
  const [assetPreviewUrls, setAssetPreviewUrls] = useState<Record<number, string>>({});
  const [assetPreviewErrors, setAssetPreviewErrors] = useState<Record<number, string>>({});
  const [downloadingAssetId, setDownloadingAssetId] = useState<number | null>(null);
  const [exportingAssetKey, setExportingAssetKey] = useState<string | null>(null);
  const assetPreviewUrlsRef = useRef<Record<number, string>>({});
  const assetTypeWatch = Form.useWatch('assetType', jobForm) as MaterialAssetType | undefined;
  const workflowStageWatch = Form.useWatch('workflowStage', jobForm) as MaterialWorkflowStage | undefined;
  const modelWatch = Form.useWatch('model', jobForm) as string | undefined;
  const normalizedAssetTypeWatch = normalizeMaterialAssetType(assetTypeWatch);

  const summaryQ = useQuery({
    queryKey: ['pm', 'material-jobs', 'summary'],
    queryFn: () => pmApi.getMaterialSummary(),
    refetchInterval: (query) => ((query.state.data?.runningJobs ?? 0) > 0 ? 2500 : false),
  });

  const materialTypeLabel = (assetType?: string) => {
    switch ((assetType || '').toLowerCase()) {
      case 'image':
        return t('operations.materialTypeImage', '图片');
      case 'music':
        return t('operations.materialTypeMusic', '音乐');
      case 'ppt':
        return t('operations.materialTypePpt', 'PPT');
      default:
        return t('operations.materialTypeText', '文案');
    }
  };

  const workflowStageLabel = (stage?: string) => {
    switch (stage) {
      case 'lyrics_draft':
        return t('operations.workflowStageMusicLyricsDraft', '歌词草案');
      case 'composition_plan':
        return t('operations.workflowStageMusicComposition', '编曲方案');
      case 'outline_draft':
        return t('operations.workflowStagePptOutlineDraft', '大纲草案');
      case 'slide_blueprint':
        return t('operations.workflowStagePptSlideBlueprint', '逐页蓝图');
      case 'visual_plan':
        return t('operations.workflowStagePptVisualPlan', '视觉方案');
      case 'generate':
        return t('operations.workflowStageGenerateSingle', '最终生成（1个）');
      default:
        return '-';
    }
  };

  const threadsQ = useQuery({
    queryKey: ['pm', 'material-threads', jobPage, jobPageSize, jobAssetTypeFilter ?? 'all', jobStatusFilter ?? 'all'],
    queryFn: () =>
      pmApi.listMaterialThreads({
        page: jobPage,
        per_page: jobPageSize,
        asset_type: jobAssetTypeFilter,
        status: jobStatusFilter,
      }),
    refetchInterval: (query) => (
      query.state.data?.items?.some((thread) => thread.status === 'running') ? 2000 : false
    ),
  });

  const expandedThreadJobsQueries = useQueries({
    queries: expandedThreadIds.map((threadId) => ({
      queryKey: ['pm', 'material-thread-jobs', threadId],
      queryFn: () =>
        pmApi.listMaterialJobs({
          page: 1,
          per_page: 200,
          thread_id: threadId,
        }),
      refetchInterval: (query: { state: { data?: { items?: MaterialJob[] } } }) => (
        query.state.data?.items?.some((job) => job.status === 'running') ? 2000 : false
      ),
    })),
  });

  const threadJobsQ = useQuery({
    queryKey: ['pm', 'material-thread-jobs', currentThreadId],
    queryFn: () =>
      currentThreadId
        ? pmApi.listMaterialJobs({
          page: 1,
          per_page: 200,
          thread_id: currentThreadId,
        })
        : Promise.resolve({ items: [], total: 0 }),
    enabled: Boolean(currentThreadId),
    refetchInterval: (query) => (
      currentThreadId && query.state.data?.items?.some((job) => job.status === 'running') ? 2000 : false
    ),
  });

  const assetsQ = useQuery({
    queryKey: ['pm', 'material-assets', currentJob?.id ?? 0],
    queryFn: () => (currentJob ? pmApi.listMaterialAssets(currentJob.id) : Promise.resolve({ items: [], total: 0 })),
    enabled: Boolean(currentJob),
    refetchInterval: (query) => {
      if (!currentJob) return false;
      if (currentJob.status === 'running') return 2000;
      // Prevent race: job may flip to completed slightly before assets become visible.
      if (
        currentJob.status === 'completed'
        && currentJob.resultCount > 0
        && (query.state.data?.items?.length ?? 0) < currentJob.resultCount
      ) {
        return 1200;
      }
      return false;
    },
  });

  const modelOptionsQ = useQuery({
    queryKey: ['pm', 'material-models', normalizedAssetTypeWatch, workflowStageWatch ?? 'none'],
    queryFn: () => pmApi.listMaterialModels({
      assetType: normalizedAssetTypeWatch,
      workflowStage: workflowStageWatch,
    }),
    enabled: jobOpen && Boolean(normalizedAssetTypeWatch),
    staleTime: 30_000,
  });

  const createJobMut = useMutation({
    mutationFn: (payload: Record<string, unknown>) => pmApi.createMaterialJob(payload as never),
    onSuccess: (created: MaterialJob) => {
      message.success(t('common.operateSuccess'));
      setJobOpen(false);
      setJobPage(1);
      jobForm.resetFields();
      setReferenceImages([]);
      const createdAssetType = (created.assetType || '').toLowerCase();
      if (continueContext || workflowStagesForAssetType(createdAssetType as MaterialAssetType).length > 0) {
        const nextThreadId = created.threadId ?? created.id;
        setCurrentThreadId(nextThreadId);
        setCurrentJob(created);
        setAssetDrawerOpen(true);
      } else {
        setCurrentJob(null);
        setCurrentThreadId(null);
        setAssetDrawerOpen(false);
      }
      setContinueContext(null);
      qc.invalidateQueries({ queryKey: ['pm', 'material-jobs'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-jobs', 'summary'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-threads'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-thread-jobs'] });
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
  });

  const deleteJobMut = useMutation({
    mutationFn: (id: number) => pmApi.deleteMaterialJob(id),
    onMutate: (id: number) => {
      setDeletingJobId(id);
    },
    onSuccess: (_resp: unknown, id: number) => {
      message.success(t('common.operateSuccess'));
      if (currentJob?.id === id) {
        setCurrentJob(null);
        setCurrentThreadId(null);
        setAssetDrawerOpen(false);
      }
      qc.invalidateQueries({ queryKey: ['pm', 'material-jobs'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-jobs', 'summary'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-threads'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-thread-jobs'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-assets'] });
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
    onSettled: () => {
      setDeletingJobId(null);
    },
  });

  const deleteThreadMut = useMutation({
    mutationFn: (id: number) => pmApi.deleteMaterialThread(id),
    onMutate: (id: number) => {
      setDeletingThreadId(id);
    },
    onSuccess: (_resp: unknown, id: number) => {
      message.success(t('operations.materialThreadDeleteSuccess', '素材任务及全部版本已删除'));
      if (currentThreadId === id) {
        setCurrentJob(null);
        setCurrentThreadId(null);
        setAssetDrawerOpen(false);
      }
      setExpandedThreadIds((prev) => prev.filter((threadId) => threadId !== id));
      qc.invalidateQueries({ queryKey: ['pm', 'material-jobs'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-jobs', 'summary'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-threads'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-thread-jobs'] });
      qc.invalidateQueries({ queryKey: ['pm', 'material-assets'] });
    },
    onError: (e: Error) => message.error(e.message || t('common.operateFailed')),
    onSettled: () => {
      setDeletingThreadId(null);
    },
  });

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['pm', 'material-jobs'] });
    qc.invalidateQueries({ queryKey: ['pm', 'material-jobs', 'summary'] });
    qc.invalidateQueries({ queryKey: ['pm', 'material-threads'] });
    qc.invalidateQueries({ queryKey: ['pm', 'material-thread-jobs'] });
    qc.invalidateQueries({ queryKey: ['pm', 'material-assets'] });
  };

  const threads: MaterialThread[] = threadsQ.data?.items ?? [];
  const threadJobs: MaterialJob[] = threadJobsQ.data?.items ?? [];
  const assets: MaterialAsset[] = assetsQ.data?.items ?? [];
  const expandedThreadJobsById = useMemo(() => {
    const map: Record<number, { items: MaterialJob[]; loading: boolean }> = {};
    expandedThreadIds.forEach((threadId, idx) => {
      const query = expandedThreadJobsQueries[idx] as {
        data?: { items?: MaterialJob[] };
        isLoading?: boolean;
        isFetching?: boolean;
      } | undefined;
      map[threadId] = {
        items: query?.data?.items ?? [],
        loading: Boolean(query?.isLoading || query?.isFetching),
      };
    });
    return map;
  }, [expandedThreadIds, expandedThreadJobsQueries]);

  const modelOptions = modelOptionsQ.data?.items ?? [];
  const noModelConfigured =
    jobOpen &&
    Boolean(normalizedAssetTypeWatch) &&
    !modelOptionsQ.isLoading &&
    modelOptions.length === 0;
  const isMusicRootCreate = normalizedAssetTypeWatch === 'music' && !continueContext;
  const referenceFileList = referenceImages.map((img, idx) => ({
    uid: img.url,
    name: img.name || `reference-${idx + 1}.png`,
    status: 'done' as const,
    url: img.url,
  }));

  const toMaterialJobFromThread = (thread: MaterialThread): MaterialJob => ({
    id: thread.latestJobId,
    missionRunId: thread.missionRunId ?? null,
    threadId: thread.threadId,
    parentJobId: null,
    iterationNo: thread.latestIterationNo,
    promptText: thread.promptText,
    model: thread.model ?? null,
    assetType: thread.assetType,
    status: thread.status,
    resultCount: thread.resultCount,
    errorMessage: thread.errorMessage ?? null,
    createdAt: thread.createdAt,
    updatedAt: thread.updatedAt,
  });

  const openThreadDetail = (thread: MaterialThread, preferredJob?: MaterialJob) => {
    setCurrentThreadId(thread.threadId);
    setCurrentJob(preferredJob ?? toMaterialJobFromThread(thread));
    setAssetDrawerOpen(true);
  };

  const openRetryComposer = (job: MaterialJob) => {
    const assetType = normalizeMaterialAssetType(job.assetType);
    jobForm.resetFields();
    jobForm.setFieldsValue({
      assetType,
      model: job.model || undefined,
      promptText: job.promptText || '',
    });
    setReferenceImages([]);
    setContinueContext(null);
    setJobOpen(true);
  };

  const inheritedPromptForJob = (baseJob: MaterialJob): string => {
    const threadId = baseJob.threadId ?? baseJob.id;
    const fromThread = [...threadJobs]
      .filter((job) => (job.threadId ?? job.id) === threadId)
      .sort((a, b) => {
        if ((a.iterationNo || 0) !== (b.iterationNo || 0)) {
          return (a.iterationNo || 0) - (b.iterationNo || 0);
        }
        return a.id - b.id;
      })
      .find((job) => job.promptText?.trim());
    return (fromThread?.promptText || baseJob.promptText || '').trim();
  };

  const openContinueComposer = (baseJob: MaterialJob, asset: MaterialAsset) => {
    const assetType = (baseJob.assetType || 'text').toLowerCase() as MaterialAssetType;
    const nextWorkflowStage = nextWorkflowStageForAsset(
      assetType,
      extractWorkflowStageFromAsset(asset),
    );
    const inheritedPrompt = inheritedPromptForJob(baseJob);
    jobForm.resetFields();
    jobForm.setFieldsValue({
      assetType,
      model: baseJob.model || undefined,
      workflowStage: nextWorkflowStage,
      promptText: inheritedPrompt,
    });
    setReferenceImages([]);
    setContinueContext({
      threadId: baseJob.threadId ?? baseJob.id,
      parentJobId: baseJob.id,
      continueFromAssetId: asset.id,
      workflowStage: nextWorkflowStage,
      promptText: inheritedPrompt,
    });
    setAssetDrawerOpen(false);
    setJobOpen(true);
  };

  const latestAssetInCurrentJob = assets.length > 0 ? assets[assets.length - 1] : null;
  const latestAssetStage = extractWorkflowStageFromAsset(latestAssetInCurrentJob ?? undefined);
  const nextStageFromLatestAsset = currentJob && latestAssetInCurrentJob
    ? nextWorkflowStageForAsset(
      (currentJob.assetType || 'text').toLowerCase() as MaterialAssetType,
      latestAssetStage,
    )
    : undefined;
  const canQuickAdvanceWorkflowStage = Boolean(
    currentJob
    && workflowStagesForAssetType((currentJob.assetType || '').toLowerCase() as MaterialAssetType).length > 0
    && currentJob.status === 'completed'
    && latestAssetInCurrentJob
    && latestAssetStage
    && latestAssetStage.toLowerCase() !== 'generate'
    && nextStageFromLatestAsset,
  );

  const handleDownloadAsset = async (asset: MaterialAsset) => {
    const downloadMarkdown = (markdown: string, filename: string) => {
      const blob = new Blob([markdown], { type: 'text/markdown;charset=utf-8' });
      saveBlob(blob, filename);
    };

    const assetType = asset.assetType.toLowerCase();
    const normalizedUrl = normalizeAssetUrl(asset.url);
    const isUrlBackedAsset = (
      assetType === 'image'
      || assetType === 'music'
      || isPptFinalDeckAsset(asset)
    ) && !!normalizedUrl;

    setDownloadingAssetId(asset.id);
    try {
      if (isUrlBackedAsset && normalizedUrl) {
        const resp = await fetch(normalizedUrl, {
          credentials: 'include',
          headers: uploadAuthHeaders(),
        });
        if (!resp.ok) {
          throw new Error(`download_failed_${resp.status}`);
        }
        const blob = await resp.blob();
        const ext = materialAssetExtension(asset, blob.type || resp.headers.get('content-type'));
        saveBlob(blob, `pm-material-asset-${asset.id}.${ext}`);
        message.success(t('operations.replyDownloadSuccess', '回复已下载'));
        return;
      }

      const markdown = buildMaterialAssetMarkdown(asset, {
        typeLabel: t('common.type', '类型'),
        contentLabel: t('common.content', '内容'),
        urlLabel: t('common.url', '链接'),
      });
      if (!markdown.trim()) {
        message.warning(t('operations.noReplyContent', '当前任务暂无可展示回复'));
        return;
      }
      downloadMarkdown(markdown, `pm-material-asset-${asset.id}.md`);
      message.success(t('operations.replyDownloadSuccess', '回复已下载'));
    } catch {
      message.error(t('operations.replyDownloadFailed', '下载回复失败'));
    } finally {
      setDownloadingAssetId(null);
    }
  };

  const handleExportPptAsset = async (asset: MaterialAsset, format: 'pdf' | 'pptx') => {
    const key = `${asset.id}:${format}`;
    setExportingAssetKey(key);
    try {
      const exported = await pmApi.exportMaterialAsset(asset.id, format);
      const normalizedUrl = normalizeAssetUrl(exported.url);
      if (!normalizedUrl) {
        throw new Error('empty_export_url');
      }
      const resp = await fetch(normalizedUrl, {
        credentials: 'include',
        headers: uploadAuthHeaders(),
      });
      if (!resp.ok) {
        throw new Error(`download_failed_${resp.status}`);
      }
      const blob = await resp.blob();
      saveBlob(blob, `pm-material-asset-${asset.id}.${format}`);
      message.success(t('operations.materialExportSuccess', '导出成功'));
    } catch (error) {
      message.error((error as Error).message || t('operations.materialExportFailed', '导出失败'));
    } finally {
      setExportingAssetKey(null);
    }
  };

  const handleDownloadThread = async () => {
    const downloadMarkdown = (markdown: string, filename: string) => {
      const blob = new Blob([markdown], { type: 'text/markdown;charset=utf-8' });
      saveBlob(blob, filename);
    };

    const threadId = currentThreadId ?? currentJob?.threadId ?? currentJob?.id ?? null;
    if (!threadId) {
      message.warning(t('operations.noMaterialThreadJobs', '当前线程暂无版本'));
      return;
    }

    try {
      const jobsResp = await pmApi.listMaterialJobs({
        page: 1,
        per_page: 200,
        thread_id: threadId,
      });
      const jobs = [...(jobsResp.items ?? [])].sort((a, b) => {
        if ((a.iterationNo || 0) !== (b.iterationNo || 0)) {
          return (a.iterationNo || 0) - (b.iterationNo || 0);
        }
        return a.id - b.id;
      });

      if (jobs.length === 0) {
        message.warning(t('operations.noMaterialThreadJobs', '当前线程暂无版本'));
        return;
      }

      const assetsByJob = await Promise.all(
        jobs.map(async (job) => {
          try {
            const resp = await pmApi.listMaterialAssets(job.id);
            const sortedAssets = [...(resp.items ?? [])].sort((a, b) => a.id - b.id);
            return { job, assets: sortedAssets, failed: false };
          } catch {
            return { job, assets: [] as MaterialAsset[], failed: true };
          }
        }),
      );

      const failedCount = assetsByJob.filter((item) => item.failed).length;
      const lines: string[] = [
        `# ${t('operations.materialThreadExportTitle', '素材线程导出')} #${threadId}`,
        '',
        `- ${t('operations.materialThreadId', '线程ID')}: ${threadId}`,
        `- ${t('operations.materialVersionCount', '共{{count}}版', { count: jobs.length })}`,
        `- ${t('operations.createdAt', '生成时间')}: ${new Date().toISOString()}`,
        '',
      ];
      assetsByJob.forEach(({ job, assets }, idx) => {
        const stageFromAsset = assets
          .map((item) => extractWorkflowStageFromAsset(item))
          .find((stage): stage is string => Boolean(stage));
        lines.push(`## ${t('operations.materialVersion', '版本')} V${job.iterationNo || idx + 1}`);
        lines.push('');
        lines.push(`- ${t('common.id', 'ID')}: ${job.id}`);
        lines.push(`- ${t('common.status', '状态')}: ${job.status}`);
        lines.push(`- ${t('common.type', '类型')}: ${materialTypeLabel(job.assetType)}`);
        lines.push(`- ${t('operations.materialWorkflowStage', '工作流阶段')}: ${workflowStageLabel(stageFromAsset)}`);
        lines.push(`- ${t('common.model', '模型')}: ${job.model || '-'}`);
        lines.push(`- ${t('common.createdAt', '创建时间')}: ${job.createdAt}`);
        lines.push(`- ${t('common.updatedAt', '更新时间')}: ${job.updatedAt}`);
        lines.push('');
        lines.push(`### ${t('operations.materialPrompt', '需求描述')}`);
        lines.push('');
        lines.push(job.promptText?.trim() || '-');
        lines.push('');
        lines.push(`### ${t('operations.materialAssets', '素材结果')}`);
        lines.push('');
        if (assets.length === 0) {
          lines.push('-');
        } else {
          assets.forEach((item) => {
            lines.push(
              buildMaterialAssetThreadSectionMarkdown(
                item,
                {
                  assetLabel: t('operations.materialAssetLabel', '素材'),
                  typeLabel: t('common.type', '类型'),
                  workflowStageLabel: t('operations.materialWorkflowStage', '工作流阶段'),
                  urlLabel: t('common.url', '链接'),
                  contentLabel: t('common.content', '内容'),
                  createdAtLabel: t('common.createdAt', '创建时间'),
                },
                workflowStageLabel(extractWorkflowStageFromAsset(item)),
              ),
            );
            lines.push('');
          });
        }
      });

      const markdown = lines.join('\n').trim();
      if (!markdown) {
        message.warning(t('operations.noReplyContent', '当前任务暂无可展示回复'));
        return;
      }
      downloadMarkdown(markdown, `pm-material-thread-${threadId}.md`);
      if (failedCount > 0) {
        message.warning(
          t(
            'operations.materialThreadDownloadPartial',
            '已下载线程 Markdown，但有 {{count}} 个阶段素材拉取失败。',
            { count: failedCount },
          ),
        );
      } else {
        message.success(t('operations.replyDownloadSuccess', '回复已下载'));
      }
    } catch {
      message.error(t('operations.replyDownloadFailed', '下载回复失败'));
    }
  };

  const handleShareAsset = (asset: MaterialAsset) => {
    const markdown = buildMaterialAssetMarkdown(asset, {
      typeLabel: t('common.type', '类型'),
      contentLabel: t('common.content', '内容'),
      urlLabel: t('common.url', '链接'),
    });
    if (!markdown.trim()) {
      message.warning(t('operations.noReplyContent', '当前任务暂无可展示回复'));
      return;
    }
    const payload: PmSharePreviewPayload = {
      schema: 'aos-pm-share-v1',
      title: `${t('operations.materialAssets', '素材结果')} - #${asset.id}`,
      generatedAt: new Date().toISOString(),
      messageId: `pm-material-asset-${asset.id}`,
      taskId: currentJob ? String(currentJob.id) : null,
      content: markdown.slice(0, PM_SHARE_PREVIEW_MAX_CHARS),
      truncated: markdown.length > PM_SHARE_PREVIEW_MAX_CHARS,
    };
    const shareUrl = buildPmSharePreviewUrl(payload);
    if (!shareUrl) {
      message.error(t('operations.replyShareOpenFailed', '打开分享预览失败'));
      return;
    }
    const opened = window.open(shareUrl, '_blank', 'noopener,noreferrer');
    if (!opened) {
      message.error(t('operations.replyShareOpenFailed', '打开分享预览失败'));
    }
  };

  useEffect(() => {
    if (!jobOpen) return;
    const workflowStages = workflowStagesForAssetType(normalizedAssetTypeWatch);
    if (workflowStages.length === 0) {
      if (jobForm.getFieldValue('workflowStage')) {
        jobForm.setFieldValue('workflowStage', undefined);
      }
      return;
    }
    const current = jobForm.getFieldValue('workflowStage') as MaterialWorkflowStage | undefined;
    if (current && workflowStages.includes(current)) return;
    const preferred = !continueContext && normalizedAssetTypeWatch === 'music'
      ? 'lyrics_draft'
      : continueContext?.workflowStage && workflowStages.includes(continueContext.workflowStage)
        ? continueContext.workflowStage
        : workflowStages[0];
    jobForm.setFieldValue('workflowStage', preferred);
  }, [continueContext, jobForm, jobOpen, normalizedAssetTypeWatch]);

  useEffect(() => {
    if (!jobOpen) return;
    if (!normalizedAssetTypeWatch || modelOptionsQ.isLoading) return;
    if (modelOptions.length === 0) {
      jobForm.setFieldValue('model', undefined);
      return;
    }
    if (!modelWatch || !modelOptions.some((opt) => opt.model === modelWatch)) {
      jobForm.setFieldValue('model', modelOptions[0].model);
    }
  }, [jobForm, jobOpen, modelOptions, modelOptionsQ.isLoading, modelWatch, normalizedAssetTypeWatch]);

  useEffect(() => {
    if (threads.length === 0) {
      if (expandedThreadIds.length > 0) setExpandedThreadIds([]);
      return;
    }
    const nextExpanded = expandedThreadIds.filter((threadId) => threads.some((thread) => thread.threadId === threadId));
    if (nextExpanded.length !== expandedThreadIds.length) {
      setExpandedThreadIds(nextExpanded);
    }
  }, [expandedThreadIds, threads]);

  useEffect(() => {
    if (!currentJob) return;
    const latestFromThread = threadJobs.find((job) => job.id === currentJob.id) ?? threadJobs[0];
    const summaryThread = threads.find((thread) => thread.threadId === (currentJob.threadId ?? currentJob.id));
    const latestFromSummary = summaryThread ? toMaterialJobFromThread(summaryThread) : null;
    const latest = latestFromThread ?? latestFromSummary;
    if (!latest) return;
    if (
      latest.updatedAt !== currentJob.updatedAt ||
      latest.status !== currentJob.status ||
      latest.resultCount !== currentJob.resultCount ||
      latest.errorMessage !== currentJob.errorMessage ||
      latest.model !== currentJob.model
    ) {
      setCurrentJob(latest);
    }
  }, [currentJob, threadJobs, threads]);

  useEffect(() => {
    if (!currentJob || currentJob.status !== 'completed') return;
    // Force one refresh when a job lands in completed, so the drawer doesn't
    // get stuck with stale "no assets" data on status flip.
    qc.invalidateQueries({ queryKey: ['pm', 'material-assets', currentJob.id] });
  }, [currentJob?.id, currentJob?.status, currentJob?.updatedAt, qc]);

  useEffect(() => {
    if (!currentThreadId) return;
    if (threadJobs.length === 0) return;
    if (!currentJob) {
      setCurrentJob(threadJobs[0]);
      return;
    }
    if (!threadJobs.some((job) => job.id === currentJob.id)) {
      setCurrentJob(threadJobs[0]);
    }
  }, [currentJob, currentThreadId, threadJobs]);

  useEffect(() => {
    assetPreviewUrlsRef.current = assetPreviewUrls;
  }, [assetPreviewUrls]);

  useEffect(() => () => {
    Object.values(assetPreviewUrlsRef.current).forEach(revokeObjectUrl);
  }, []);

  useEffect(() => {
    if (assetDrawerOpen) return;
    setAssetPreviewUrls((prev) => {
      Object.values(prev).forEach(revokeObjectUrl);
      return {};
    });
    setAssetPreviewErrors({});
  }, [assetDrawerOpen]);

  useEffect(() => {
    if (!assetDrawerOpen) return;
    const previewableAssets = assets
      .map((asset) => ({
        id: asset.id,
        assetType: asset.assetType.toLowerCase(),
        url: normalizeAssetUrl(asset.url),
        finalPptDeck: isPptFinalDeckAsset(asset),
      }))
      .filter(
        (asset): asset is { id: number; assetType: string; url: string; finalPptDeck: boolean } =>
          (asset.assetType === 'image' || asset.assetType === 'music' || asset.finalPptDeck) && Boolean(asset.url),
      );

    const activeIds = new Set(previewableAssets.map((asset) => asset.id));
    setAssetPreviewUrls((prev) => {
      const next: Record<number, string> = {};
      let changed = false;
      for (const [idStr, url] of Object.entries(prev)) {
        const id = Number(idStr);
        if (activeIds.has(id)) {
          next[id] = url;
        } else {
          changed = true;
          revokeObjectUrl(url);
        }
      }
      if (!changed && Object.keys(next).length === Object.keys(prev).length) {
        return prev;
      }
      return next;
    });
    setAssetPreviewErrors((prev) => {
      const next: Record<number, string> = {};
      let changed = false;
      for (const [idStr, error] of Object.entries(prev)) {
        if (activeIds.has(Number(idStr))) {
          next[Number(idStr)] = error;
        } else {
          changed = true;
        }
      }
      if (!changed && Object.keys(next).length === Object.keys(prev).length) {
        return prev;
      }
      return next;
    });

    if (previewableAssets.length === 0) return;
    const headers = uploadAuthHeaders();
    const controllers: AbortController[] = [];
    let disposed = false;

    const load = async () => {
      for (const asset of previewableAssets) {
        if (assetPreviewUrls[asset.id]) continue;
        const controller = new AbortController();
        controllers.push(controller);
        try {
          const resp = await fetch(asset.url, {
            method: 'GET',
            headers,
            signal: controller.signal,
          });
          if (!resp.ok) {
            throw new Error(`HTTP ${resp.status}`);
          }
          const blob = await resp.blob();
          const objectUrl = URL.createObjectURL(blob);
          if (disposed) {
            revokeObjectUrl(objectUrl);
            return;
          }
          setAssetPreviewUrls((prev) => {
            if (prev[asset.id]) {
              revokeObjectUrl(objectUrl);
              return prev;
            }
            return { ...prev, [asset.id]: objectUrl };
          });
          setAssetPreviewErrors((prev) => {
            if (!prev[asset.id]) return prev;
            const next = { ...prev };
            delete next[asset.id];
            return next;
          });
        } catch (error) {
          if (controller.signal.aborted || disposed) continue;
          setAssetPreviewErrors((prev) => ({
            ...prev,
            [asset.id]: (error as Error).message || 'preview_load_failed',
          }));
        }
      }
    };
    load();

    return () => {
      disposed = true;
      controllers.forEach((controller) => controller.abort());
    };
  }, [assetDrawerOpen, assets, assetPreviewUrls]);

  return (
    <div style={{ padding: '24px 24px 0', minWidth: 0, maxWidth: '100%', overflowX: 'hidden' }}>
      <Card
        title={t('operations.materialsTitle', '素材工坊')}
        extra={<Button onClick={refresh}>{t('operations.ui.buttons.refresh')}</Button>}
        styles={{ body: { minWidth: 0, overflow: 'hidden' } }}
      >
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 12 }}
          message={t(
            'operations.materialsDesc',
            '输入投放目标或活动需求，系统自动生成可复用素材内容（文案/创意说明）。',
          )}
        />

        <Row gutter={[12, 12]} style={{ marginBottom: 12 }}>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.materialMetricThreads', 'Threads')}
                value={summaryQ.data?.totalThreads ?? 0}
                suffix={`/ ${summaryQ.data?.totalJobs ?? 0}`}
                loading={summaryQ.isLoading}
                valueStyle={{ fontSize: 22 }}
              />
            </Card>
          </Col>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.materialMetricRunning', 'Running')}
                value={summaryQ.data?.runningJobs ?? 0}
                loading={summaryQ.isLoading}
                valueStyle={{ fontSize: 22, color: '#1677ff' }}
              />
            </Card>
          </Col>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.materialMetricSuccess30d', '30d Success')}
                value={percentText(summaryQ.data?.successRate30d)}
                loading={summaryQ.isLoading}
                valueStyle={{ fontSize: 22, color: '#3f8600' }}
              />
              <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                {t('operations.statusFailed', 'Failed')}: {summaryQ.data?.failedJobs30d ?? 0}
              </Typography.Text>
            </Card>
          </Col>
          <Col xs={12} md={6}>
            <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
              <Statistic
                title={t('operations.materialMetricAssets30d', 'Outputs 30d')}
                value={summaryQ.data?.assetCount30d ?? 0}
                loading={summaryQ.isLoading}
                valueStyle={{ fontSize: 22 }}
              />
              <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                {t('operations.materialMetricMix30d', '文案/图片/音乐/PPT')}: {summaryQ.data?.textJobs30d ?? 0}/
                {summaryQ.data?.imageJobs30d ?? 0}/{summaryQ.data?.musicJobs30d ?? 0}/{summaryQ.data?.pptJobs30d ?? 0}
              </Typography.Text>
            </Card>
          </Col>
        </Row>

        <RowActions>
          <Button
            type="primary"
            icon={<PlusOutlined />}
            onClick={() => {
              jobForm.resetFields();
              jobForm.setFieldsValue({ assetType: 'text' });
              setReferenceImages([]);
              setContinueContext(null);
              setJobOpen(true);
            }}
          >
            {t('operations.createMaterialJob', '新建素材任务')}
          </Button>
          <Select
            allowClear
            style={{ minWidth: 180 }}
            placeholder={`${t('common.type', '类型')}`}
            value={jobAssetTypeFilter}
            onChange={(value) => {
              setJobAssetTypeFilter((value as MaterialAssetType | undefined) ?? undefined);
              setJobPage(1);
            }}
            options={[
              ...MATERIAL_FILTER_ASSET_TYPES.map((value) => ({
                value,
                label: materialTypeLabel(value),
              })),
            ]}
          />
          <Select
            allowClear
            style={{ minWidth: 180 }}
            placeholder={`${t('common.status', '状态')}`}
            value={jobStatusFilter}
            onChange={(value) => {
              setJobStatusFilter((value as string | undefined) ?? undefined);
              setJobPage(1);
            }}
            options={[
              { value: 'queued', label: t('operations.statusQueued', '排队中') },
              { value: 'running', label: t('operations.statusRunning', '运行中') },
              { value: 'completed', label: t('operations.statusCompleted', '已完成') },
              { value: 'failed', label: t('operations.statusFailed', '失败') },
            ]}
          />
        </RowActions>

        <Table
          rowKey="threadId"
          tableLayout="fixed"
          scroll={{ x: 1400 }}
          loading={threadsQ.isLoading}
          dataSource={threads}
          locale={{ emptyText: t('operations.noMaterialJobs', '暂无素材任务') }}
          pagination={{
            current: jobPage,
            pageSize: jobPageSize,
            total: threadsQ.data?.total ?? 0,
            showSizeChanger: true,
            onChange: (page, pageSize) => {
              setJobPage(page);
              setJobPageSize(pageSize);
            },
          }}
          expandable={{
            expandedRowKeys: expandedThreadIds,
            onExpand: (expanded, row) => {
              setExpandedThreadIds((prev) => {
                if (expanded) {
                  if (prev.includes(row.threadId)) return prev;
                  return [...prev, row.threadId];
                }
                return prev.filter((id) => id !== row.threadId);
              });
            },
            rowExpandable: (row) => row.versionCount > 0,
            expandedRowRender: (row) => {
              const versionState = expandedThreadJobsById[row.threadId] ?? { items: [], loading: false };
              return (
                <div style={{ width: '100%', maxWidth: '100%', minWidth: 0, overflowX: 'auto' }}>
                  <Table
                    size="small"
                    rowKey="id"
                    tableLayout="fixed"
                    scroll={{ x: 900 }}
                    loading={versionState.loading}
                    dataSource={versionState.items}
                    pagination={false}
                    locale={{ emptyText: t('operations.noMaterialThreadJobs', '当前线程暂无版本') }}
                    columns={[
                    { title: t('common.id', 'ID'), dataIndex: 'id', width: 72 },
                    {
                      title: t('operations.materialVersion', '版本'),
                      dataIndex: 'iterationNo',
                      width: 88,
                      render: (v: number) => `V${v || 1}`,
                    },
                    { title: t('common.description', '描述'), dataIndex: 'promptText', ellipsis: true },
                    {
                      title: t('common.status', '状态'),
                      dataIndex: 'status',
                      width: 110,
                      render: (v: string) =>
                        v === 'completed'
                          ? <Tag color="green">{t('operations.statusCompleted', '已完成')}</Tag>
                          : v === 'failed'
                            ? <Tag color="red">{t('operations.statusFailed', '失败')}</Tag>
                            : <Tag color="blue">{t('operations.statusRunning', '运行中')}</Tag>,
                    },
                    { title: t('common.createdAt', '创建时间'), dataIndex: 'createdAt', width: 180 },
                    {
                      title: t('common.actions', '操作'),
                      width: 180,
                      render: (_: unknown, versionRow: MaterialJob) => (
                        <Space wrap>
                          <Button
                            size="small"
                            onClick={() => openThreadDetail(row, versionRow)}
                          >
                            {t('common.viewDetail', '查看详情')}
                          </Button>
                          {versionRow.status === 'failed' ? (
                            <Button
                              size="small"
                              icon={<ReloadOutlined />}
                              onClick={() => openRetryComposer(versionRow)}
                            >
                              {t('operations.retry', 'Retry')}
                            </Button>
                          ) : null}
                          <Popconfirm
                            title={t('common.deleteConfirm', '确认删除吗？')}
                            onConfirm={() => deleteJobMut.mutate(versionRow.id)}
                            okText={t('common.confirm', '确定')}
                            cancelText={t('common.cancel', '取消')}
                          >
                            <Button
                              size="small"
                              danger
                              icon={<DeleteOutlined />}
                              disabled={versionRow.status === 'running'}
                              title={
                                versionRow.status === 'running'
                                  ? t('operations.materialRunningDeleteDisabled', 'Running material jobs cannot be deleted until generation settles.')
                                  : undefined
                              }
                              loading={deleteJobMut.isPending && deletingJobId === versionRow.id}
                            />
                          </Popconfirm>
                        </Space>
                      ),
                    },
                    ]}
                  />
                </div>
              );
            },
          }}
          columns={[
            { title: t('operations.materialThreadId', '线程ID'), dataIndex: 'threadId', width: 100 },
            {
              title: t('operations.materialLatestJobId', '最新记录ID'),
              dataIndex: 'latestJobId',
              width: 108,
            },
            {
              title: t('operations.materialVersion', '版本'),
              width: 150,
              render: (_: unknown, row: MaterialThread) =>
                `V${row.latestIterationNo || 1} · ${t('operations.materialVersionCount', '共{{count}}版', { count: row.versionCount })}`,
            },
            {
              title: t('common.type'),
              dataIndex: 'assetType',
              width: 110,
              render: (v: string) => <Tag>{materialTypeLabel(v)}</Tag>,
            },
            { title: t('common.description'), dataIndex: 'promptText', ellipsis: true },
            { title: t('common.model', '模型'), dataIndex: 'model', width: 180, render: (v?: string | null) => v || '-' },
            {
              title: t('common.status'),
              dataIndex: 'status',
              width: 110,
              render: (v: string) =>
                v === 'completed'
                  ? <Tag color="green">{t('operations.statusCompleted', '已完成')}</Tag>
                  : v === 'failed'
                    ? <Tag color="red">{t('operations.statusFailed', '失败')}</Tag>
                    : <Tag color="blue">{t('operations.statusRunning', '运行中')}</Tag>,
            },
            { title: t('common.createdAt'), dataIndex: 'updatedAt', width: 180 },
            {
              title: t('common.actions'),
              width: 250,
              render: (_: unknown, row: MaterialThread) => (
                <Space wrap>
                  <Button
                    size="small"
                    onClick={() => {
                      openThreadDetail(row);
                    }}
                  >
                    {t('common.viewDetail')}
                  </Button>
                  {row.status === 'failed' ? (
                    <Button
                      size="small"
                      icon={<ReloadOutlined />}
                      onClick={() => openRetryComposer(toMaterialJobFromThread(row))}
                    >
                      {t('operations.retry', 'Retry')}
                    </Button>
                  ) : null}
                  <Popconfirm
                    title={t('operations.materialThreadDeleteConfirm', '确认删除整个素材任务及其全部版本吗？')}
                    onConfirm={() => deleteThreadMut.mutate(row.threadId)}
                    okText={t('common.confirm')}
                    cancelText={t('common.cancel')}
                  >
                    <Button
                      size="small"
                      danger
                      icon={<DeleteOutlined />}
                      disabled={row.status === 'running'}
                      title={
                        row.status === 'running'
                          ? t('operations.materialRunningDeleteDisabled', 'Running material jobs cannot be deleted until generation settles.')
                          : undefined
                      }
                      loading={deleteThreadMut.isPending && deletingThreadId === row.threadId}
                    />
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      </Card>

      <Modal
        title={
          continueContext
            ? t('operations.materialContinueJob', '继续修改素材')
            : t('operations.createMaterialJob', '新建素材任务')
        }
        open={jobOpen}
        onCancel={() => {
          setJobOpen(false);
          setContinueContext(null);
          setReferenceImages([]);
        }}
        onOk={() => jobForm.submit()}
        confirmLoading={createJobMut.isPending}
        okButtonProps={{
          disabled: referenceUploading || modelOptionsQ.isLoading || noModelConfigured,
        }}
      >
        <Form
          form={jobForm}
          layout="vertical"
          onFinish={(values) => {
            const normalizedAssetType = normalizeMaterialAssetType(values.assetType);
            if (!values.model) {
              message.warning(
                t(
                  'operations.materialModelMissingConfig',
                  '当前类型未配置可用模型，请先到 API 密钥管理中创建并启用对应模型。',
                ),
              );
              return;
            }
            const workflowStage = normalizedAssetType === 'music' && !continueContext
              ? 'lyrics_draft'
              : values.workflowStage;
            const effectivePromptText = String(values.promptText ?? '').trim()
              || continueContext?.promptText
              || '';
            createJobMut.mutate({
              ...values,
              promptText: effectivePromptText,
              threadId: continueContext?.threadId,
              parentJobId: continueContext?.parentJobId,
              continueFromAssetId: continueContext?.continueFromAssetId,
              workflowStage,
              referenceImages: normalizedAssetType === 'image' ? referenceImages : [],
            });
          }}
        >
          {continueContext ? (
            <Alert
              showIcon
              type="info"
              style={{ marginBottom: 16 }}
              message={t(
                'operations.materialContinueHint',
                '当前是版本迭代模式：系统已带入原始需求，并会基于上一阶段结果继续推进；你可以直接提交，也可以补充修改要求。',
              )}
            />
          ) : null}
          {isMusicRootCreate ? (
            <Alert
              showIcon
              type="info"
              style={{ marginBottom: 16 }}
              message={t(
                'operations.materialMusicFlowHint',
                '音乐任务采用三阶段工作流：歌词草案 -> 编曲方案 -> 最终生成',
              )}
              description={t(
                'operations.materialMusicFlowHintDesc',
                '新建任务固定从“歌词草案”开始，每个阶段只产出1个结果，不提供A/B分支。',
              )}
            />
          ) : null}
          <Form.Item name="assetType" label={t('common.type')} rules={[{ required: true }]}>
            <Select
              disabled={Boolean(continueContext)}
              options={MATERIAL_CREATE_ASSET_TYPES.map((value) => ({
                value,
                label: materialTypeLabel(value),
              }))}
            />
          </Form.Item>
          {workflowStagesForAssetType(normalizedAssetTypeWatch).length > 0 ? (
            <Form.Item
              name="workflowStage"
              label={t('operations.materialWorkflowStage', '工作流阶段')}
              rules={[{ required: true }]}
              extra={isMusicRootCreate
                ? t(
                  'operations.materialWorkflowStageHintMusicLocked',
                  '音乐新建任务固定从“歌词草案”开始，每个阶段只输出1个结果，不支持A/B。',
                )
                : t('operations.materialWorkflowStageHint', '按阶段逐步产出，生成阶段固定只输出 1 个结果。')}
            >
              <Select
                disabled={isMusicRootCreate}
                options={workflowStagesForAssetType(normalizedAssetTypeWatch).map((stage) => ({
                  value: stage,
                  label: workflowStageLabel(stage),
                }))}
              />
            </Form.Item>
          ) : null}
          {modelOptionsQ.isLoading ? (
            <Alert
              showIcon
              type="info"
              style={{ marginBottom: 16 }}
              message={t('operations.materialModelLoading', '正在加载可用模型...')}
            />
          ) : modelOptions.length > 0 ? (
            <Form.Item name="model" label={t('common.model', '模型')} rules={[{ required: true }]}>
              <Select
                options={modelOptions.map((item) => ({ value: item.model, label: item.model }))}
                placeholder={t('operations.materialModelSelectPlaceholder', '请选择模型')}
              />
            </Form.Item>
          ) : (
            <Alert
              showIcon
              type="warning"
              style={{ marginBottom: 16 }}
              message={t(
                'operations.materialModelMissingConfig',
                '当前类型未配置可用模型，请先到 API 密钥管理中创建并启用对应模型。',
              )}
              description={
                <a href="/keys">
                  {t('apikeys.goToConfig', '去配置密钥')}
                </a>
              }
            />
          )}
          {normalizedAssetTypeWatch === 'image' ? (
            <Form.Item
              label={t('operations.materialReferenceImages', '参考图')}
              extra={t(
                'operations.materialReferenceImagesHint',
                '最多上传3张参考图，系统将作为风格与构图参考。',
              )}
            >
              <Upload
                accept="image/*"
                multiple
                fileList={referenceFileList}
                beforeUpload={async (file) => {
                  if (!file.type.startsWith('image/')) {
                    message.warning(t('operations.pmBackgroundImagesOnly', '仅支持图片文件'));
                    return Upload.LIST_IGNORE;
                  }
                  if (referenceImages.length >= 3) {
                    message.warning(
                      t(
                        'operations.materialReferenceImageLimit',
                        '参考图最多上传3张。',
                      ),
                    );
                    return Upload.LIST_IGNORE;
                  }
                  setReferenceUploading(true);
                  try {
                    const uploaded = await uploadFile(file as File);
                    setReferenceImages((prev) => {
                      if (prev.length >= 3) return prev;
                      return [
                        ...prev,
                        {
                          url: uploaded.url,
                          mediaType: uploaded.mediaType,
                          name: uploaded.filename,
                          sizeBytes: uploaded.size,
                        },
                      ];
                    });
                    message.success(t('chat.uploadSuccess', '上传成功'));
                  } catch (err) {
                    message.error(`${t('chat.uploadFailed', '上传失败')}: ${(err as Error).message}`);
                  } finally {
                    setReferenceUploading(false);
                  }
                  return Upload.LIST_IGNORE;
                }}
                onRemove={(file) => {
                  const key = file.url || file.uid;
                  setReferenceImages((prev) => prev.filter((img) => img.url !== key));
                  return true;
                }}
              >
                <Button icon={<UploadOutlined />} loading={referenceUploading}>
                  {t('operations.materialUploadImage', '上传图片')}
                </Button>
              </Upload>
            </Form.Item>
          ) : null}
          <Form.Item
            name="promptText"
            label={continueContext
              ? t('operations.materialContinuePrompt', '原始需求 / 追加修改')
              : t('operations.materialPrompt', '需求描述')}
            rules={continueContext ? [] : [{ required: true }]}
            extra={continueContext
              ? t('operations.materialContinuePromptHint', '已自动带入上一阶段的需求；不需要重复填写，必要时只追加你想调整的方向。')
              : undefined}
          >
            <Input.TextArea
              rows={5}
              placeholder={continueContext
                ? t('operations.materialContinuePromptPlaceholder', '可直接提交进入下一阶段；也可以补充：比如更商务、更数据化、减少文字等。')
                : t('operations.materialPromptPlaceholder', '例如：为印尼市场做一套“高留存新人礼包”活动素材，面向18-25岁用户。')}
            />
          </Form.Item>
        </Form>
      </Modal>

      <Drawer
        title={
          currentJob
            ? `${t('operations.materialAssets', '素材结果')} · #${currentJob.id} · ${
              t('operations.materialVersion', '版本')
            } V${currentJob.iterationNo || 1}`
            : t('operations.materialAssets', '素材结果')
        }
        extra={(
          <Space>
            {currentThreadId || currentJob ? (
              <Button
                icon={<DownloadOutlined />}
                onClick={() => void handleDownloadThread()}
              >
                {t('operations.materialThreadExportTitle', '素材线程导出')}
              </Button>
            ) : null}
            {canQuickAdvanceWorkflowStage && currentJob && latestAssetInCurrentJob ? (
              <Button
                type="primary"
                onClick={() => openContinueComposer(currentJob, latestAssetInCurrentJob)}
              >
                {t('operations.materialNextStageAction', '进入下一阶段')}
              </Button>
            ) : null}
          </Space>
        )}
        open={assetDrawerOpen}
        onClose={() => {
          setAssetDrawerOpen(false);
          setCurrentThreadId(null);
          setCurrentJob(null);
        }}
        width={860}
      >
        {currentThreadId ? (
          <Table
            size="small"
            rowKey="id"
            loading={threadJobsQ.isLoading}
            dataSource={threadJobs}
            pagination={false}
            style={{ marginBottom: 12 }}
            locale={{ emptyText: t('operations.noMaterialThreadJobs', '当前线程暂无版本') }}
            columns={[
              { title: t('common.id', 'ID'), dataIndex: 'id', width: 72 },
              {
                title: t('operations.materialVersion', '版本'),
                dataIndex: 'iterationNo',
                width: 88,
                render: (v: number) => `V${v || 1}`,
              },
              {
                title: t('common.status'),
                dataIndex: 'status',
                width: 110,
                render: (v: string) =>
                  v === 'completed'
                    ? <Tag color="green">{t('operations.statusCompleted', '已完成')}</Tag>
                    : v === 'failed'
                      ? <Tag color="red">{t('operations.statusFailed', '失败')}</Tag>
                      : <Tag color="blue">{t('operations.statusRunning', '运行中')}</Tag>,
              },
              {
                title: t('common.createdAt', '创建时间'),
                dataIndex: 'createdAt',
                width: 168,
              },
              {
                title: t('common.actions', '操作'),
                width: 84,
                render: (_: unknown, row: MaterialJob) => (
                  <Button
                    size="small"
                    type={currentJob?.id === row.id ? 'primary' : 'default'}
                    onClick={() => setCurrentJob(row)}
                  >
                    {t('common.viewDetail', '查看详情')}
                  </Button>
                ),
              },
            ]}
          />
        ) : null}

        <Table
          rowKey="id"
          loading={assetsQ.isLoading}
          dataSource={assets}
          pagination={false}
          tableLayout="fixed"
          locale={{ emptyText: t('operations.noMaterialAssets', '暂无素材结果') }}
          columns={[
            { title: t('common.id', 'ID'), dataIndex: 'id', width: 80 },
            {
              title: t('common.type'),
              dataIndex: 'assetType',
              width: 100,
              render: (v: string) => materialTypeLabel(v),
            },
            {
              title: t('operations.materialWorkflowStage', '工作流阶段'),
              width: 140,
              render: (_: unknown, row: MaterialAsset) => workflowStageLabel(extractWorkflowStageFromAsset(row)),
            },
            {
              title: t('common.content', '内容'),
              dataIndex: 'contentText',
              render: (v: string | null | undefined, row: MaterialAsset) => {
                const normalizedUrl = normalizeAssetUrl(row.url);
                const assetType = row.assetType.toLowerCase();
                const isImage = assetType === 'image';
                const isMusic = assetType === 'music';
                const isPpt = assetType === 'ppt';
                const isPptDeck = isPptFinalDeckAsset(row);
                if (isPpt) {
                  const previewSrc = assetPreviewUrls[row.id] || normalizedUrl || undefined;
                  return (
                    <Space direction="vertical" size={8} style={{ width: '100%' }}>
                      {isPptDeck && previewSrc ? (
                        <iframe
                          title={`ppt-material-asset-${row.id}`}
                          src={previewSrc}
                          sandbox="allow-scripts allow-same-origin allow-popups"
                          style={{
                            width: '100%',
                            maxWidth: 560,
                            aspectRatio: '16 / 9',
                            border: '1px solid #d9d9d9',
                            borderRadius: 8,
                            background: '#fff',
                          }}
                        />
                      ) : null}
                      {isPptDeck && normalizedUrl ? (
                        <a href={normalizedUrl} target="_blank" rel="noreferrer">
                          {normalizedUrl}
                        </a>
                      ) : null}
                      {isPptDeck && assetPreviewErrors[row.id] ? (
                        <Text type="secondary">
                          {t('operations.materialPptPreviewLoadFailed', 'PPT 已生成，但预览加载失败，可使用下载或导出查看。')}
                        </Text>
                      ) : null}
                      {!isPptDeck && v ? (
                        <div style={MATERIAL_CONTENT_BOX_STYLE}>
                          <Markdown>{v}</Markdown>
                        </div>
                      ) : null}
                    </Space>
                  );
                }
                if (isImage || isMusic) {
                  const previewSrc = assetPreviewUrls[row.id] || normalizedUrl || undefined;
                  return (
                    <Space direction="vertical" size={8} style={{ width: '100%' }}>
                      {previewSrc ? isImage ? (
                        <Image
                          src={previewSrc}
                          alt={`material-asset-${row.id}`}
                          style={{ maxWidth: 260, borderRadius: 8 }}
                        />
                      ) : (
                        <audio
                          controls
                          preload="none"
                          src={previewSrc}
                          style={{ maxWidth: 360, width: '100%' }}
                        />
                      ) : (
                        <Text type="secondary">-</Text>
                      )}
                      {normalizedUrl ? (
                        <a href={normalizedUrl} target="_blank" rel="noreferrer">
                          {normalizedUrl}
                        </a>
                      ) : null}
                      {assetPreviewErrors[row.id] ? (
                        <Text type="secondary">
                          {t('operations.materialMediaPreviewLoadFailed', '媒体已生成，但预览加载失败，可使用下载或分享查看。')}
                        </Text>
                      ) : null}
                      {v ? (
                        <div style={MATERIAL_MEDIA_TEXT_BOX_STYLE}>
                          <Markdown>{v}</Markdown>
                        </div>
                      ) : null}
                    </Space>
                  );
                }
                return v ? (
                  <div style={MATERIAL_CONTENT_BOX_STYLE}>
                    <Markdown>{v}</Markdown>
                  </div>
                ) : (
                  <Text type="secondary">-</Text>
                );
              },
            },
            {
              title: t('common.actions', '操作'),
              width: 380,
              render: (_: unknown, row: MaterialAsset) => {
                const isPptDeck = isPptFinalDeckAsset(row);
                return (
                  <Space wrap>
                    {row.assetType.toLowerCase() === 'text' && row.contentText?.trim() ? (
                      <Button
                        size="small"
                        icon={<EyeOutlined />}
                        onClick={() => setViewingTextAsset(row)}
                      >
                        {t('common.viewDetail', '查看')}
                      </Button>
                    ) : null}
                    {row.assetType.toLowerCase() !== 'ppt' ? (
                      <Button
                        size="small"
                        icon={<EditOutlined />}
                        onClick={() => {
                          if (!currentJob) return;
                          openContinueComposer(currentJob, row);
                        }}
                      >
                        {t('operations.materialContinueAction', '继续修改')}
                      </Button>
                    ) : null}
                    <Button
                      size="small"
                      icon={<DownloadOutlined />}
                      loading={downloadingAssetId === row.id}
                      onClick={() => void handleDownloadAsset(row)}
                    >
                      {isPptDeck ? t('operations.materialDownloadHtml', '下载 HTML') : t('operations.download', '下载')}
                    </Button>
                    {isPptDeck ? (
                      <>
                        <Button
                          size="small"
                          icon={<FilePdfOutlined />}
                          loading={exportingAssetKey === `${row.id}:pdf`}
                          onClick={() => void handleExportPptAsset(row, 'pdf')}
                        >
                          {t('operations.materialExportPdf', '导出 PDF')}
                        </Button>
                        <Button
                          size="small"
                          icon={<FilePptOutlined />}
                          loading={exportingAssetKey === `${row.id}:pptx`}
                          onClick={() => void handleExportPptAsset(row, 'pptx')}
                        >
                          {t('operations.materialExportPptx', '导出 PPTX')}
                        </Button>
                      </>
                    ) : null}
                    <Button size="small" icon={<ShareAltOutlined />} onClick={() => handleShareAsset(row)}>
                      {t('operations.share', '分享')}
                    </Button>
                  </Space>
                );
              },
            },
          ]}
        />
      </Drawer>
      <Modal
        title={t('operations.materialTextPreviewTitle', '文案内容')}
        open={Boolean(viewingTextAsset)}
        footer={null}
        width={860}
        onCancel={() => setViewingTextAsset(null)}
        destroyOnHidden
      >
        <div style={{ maxHeight: '70vh', overflowY: 'auto', padding: '4px 8px' }}>
          <Markdown relaxed>{viewingTextAsset?.contentText ?? ''}</Markdown>
        </div>
      </Modal>
    </div>
  );
}

function RowActions({ children }: { children: ReactNode }) {
  return (
    <Space style={{ marginBottom: 12 }}>
      {children}
    </Space>
  );
}
