import { useState, useCallback, useMemo, useRef, useEffect } from 'react';
import { useNavigate } from '@/router';
import {
  Card,
  Table,
  Tag,
  Typography,
  Input,
  Button,
  Modal,
  Form,
  message,
  Space,
  Drawer,
  Switch,
  Popconfirm,
  Select,
  Tooltip,
  Row,
  Col,
  Statistic,
  Alert,
  Upload,
  Tabs,
  Descriptions,
  Divider,
  Segmented,
  Spin,
  Checkbox,
} from 'antd';
import type { ColumnsType } from 'antd/es/table';
import { useQuery, useMutation, useQueryClient, useInfiniteQuery } from '@tanstack/react-query';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import { useTranslation } from 'react-i18next';
import MonacoEditor from '@monaco-editor/react';
import { skillsApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { useSystemEvents } from '@/api/systemEvents';
import { ApiError } from '@/api/errors';
import { useTabRefresh } from '@/hooks/useTabRefresh';
import { PageSkeleton } from '@/components/Skeleton';
import { Markdown } from '@/components/chat';
import type { SkillInfo, SkillSecurityScan } from '@/types';
import { usePermissions } from '@/store/permissions';
import {
  SearchOutlined,
  EyeOutlined,
  PlusOutlined,
  UploadOutlined,
  LinkOutlined,
  DeleteOutlined,
  ReloadOutlined,
  SyncOutlined,
  CheckCircleOutlined,
  FileZipOutlined,
  WarningOutlined,
  InboxOutlined,
  StarOutlined,
  EditOutlined,
  SaveOutlined,
  CodeOutlined,
  FolderOutlined,
  ThunderboltOutlined,
  SettingOutlined,
} from '@ant-design/icons';

dayjs.extend(relativeTime);

const { Title, Text, Paragraph } = Typography;

const SOURCE_COLORS: Record<string, string> = {
  uploaded: 'blue',
  marketplace: 'purple',
  builtin: 'green',
};

type SkillMarketRepository = {
  id: string;
  tenantId?: string | null;
  repoFullName: string;
  repoUrl: string;
  branch: string;
  enabled: boolean;
  discoveredCount: number;
  lastScanAt?: string | null;
  lastScanStatus: string;
  lastScanError?: string | null;
  createdBy?: string | null;
  createdAt?: string | null;
  updatedAt?: string | null;
  builtIn: boolean;
};

type SkillMarketSearchItem = {
  id: string;
  repoFullName: string;
  repoUrl: string;
  branch: string;
  skillName: string;
  skillPath: string;
  readmeUrl?: string | null;
  htmlUrl?: string | null;
  sourceType: string;
};

// ── Skill Commands Tab ────────────────────────────────────────────────────────

function SkillCommandsTab({ skillName, commandsCount }: { skillName: string; commandsCount: number }) {
  const { t } = useTranslation();
  const { data: commands, isLoading } = useQuery({
    queryKey: ['skills', skillName, 'commands'],
    queryFn: () => skillsApi.commands(skillName),
    enabled: commandsCount > 0,
  });

  if (commandsCount === 0) {
    return <Alert message={t('skills.noCommands')} type="info" showIcon />;
  }

  if (isLoading) {
    return <div style={{ textAlign: 'center', padding: 24 }}><Text type="secondary">{t('common.loading')}</Text></div>;
  }

  if (!commands?.length) {
    return <Alert message={t('skills.noCommands')} type="info" showIcon />;
  }

  return (
    <Table
      dataSource={commands}
      rowKey="name"
      size="small"
      pagination={false}
      columns={[
        {
          title: t('skills.commandName'),
          dataIndex: 'name',
          key: 'name',
          render: (v: string) => <code style={{ fontSize: 12 }}>{v}</code>,
        },
        {
          title: t('skills.commandSize'),
          dataIndex: 'size',
          key: 'size',
          width: 100,
          align: 'right',
          render: (v?: number) => v != null ? `${(v / 1024).toFixed(1)} KB` : '—',
        },
      ]}
    />
  );
}

// ── Main component ────────────────────────────────────────────────────────────

export default function Skills() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { hasPermission } = usePermissions();
  const canWrite = hasPermission('skills:write');
  const canDelete = hasPermission('skills:delete');
  const { connected } = useSystemEvents();

  // ── Filters ────────────────────────────────────────────────────────────────
  const [activeView, setActiveView] = useState<'installed' | 'market' | 'repositories'>('installed');
  const [keyword, setKeyword] = useState('');
  const [sourceFilter, setSourceFilter] = useState<string | undefined>(undefined);
  const [statusFilter, setStatusFilter] = useState<boolean | undefined>(undefined);
  const [marketSearchInput, setMarketSearchInput] = useState('');
  const [marketKeyword, setMarketKeyword] = useState('');
  const [installedMarketItemHints, setInstalledMarketItemHints] = useState<string[]>([]);
  const [installingMarketItemId, setInstallingMarketItemId] = useState<string | null>(null);
  const [scanningRepoIds, setScanningRepoIds] = useState<string[]>([]);
  const [deletingRepoIds, setDeletingRepoIds] = useState<string[]>([]);
  const [repoUrlInput, setRepoUrlInput] = useState('');
  const [repoBranchInput, setRepoBranchInput] = useState('main');
  const MARKET_PAGE_SIZE = 20;
  const REPO_PAGE_SIZE = 20;
  const marketScrollRef = useRef<HTMLDivElement | null>(null);
  const repoScrollRef = useRef<HTMLDivElement | null>(null);

  // ── Upload modal ─────────────────────────────────────────────────────────
  const [uploadModalOpen, setUploadModalOpen] = useState(false);
  const [uploadForm, setUploadForm] = useState({ name: '', description: '', tags: [] as string[] });
  const [selectedZip, setSelectedZip] = useState<File | null>(null);
  const [uploading, setUploading] = useState(false);
  const [zipSecurityScan, setZipSecurityScan] = useState<SkillSecurityScan | null>(null);
  const [zipScanning, setZipScanning] = useState(false);
  const [riskConfirmed, setRiskConfirmed] = useState(false);

  // ── Edit skill modal ─────────────────────────────────────────────────────
  const [editModalOpen, setEditModalOpen] = useState(false);
  const [editingSkill, setEditingSkill] = useState<SkillInfo | null>(null);
  const [editForm, setEditForm] = useState({ name: '', description: '', tags: [] as string[] });

  // ── README drawer ─────────────────────────────────────────────────────────
  const [readmeDrawerOpen, setReadmeDrawerOpen] = useState(false);
  const [readmeName, setReadmeName] = useState('');
  const [readmeContent, setReadmeContent] = useState<string | null>(null);
  const [readmeLoading, setReadmeLoading] = useState(false);
  const [readmeEditMode, setReadmeEditMode] = useState(false);
  const [readmeEditValue, setReadmeEditValue] = useState('');
  const [readmeSaving, setReadmeSaving] = useState(false);
  const [selectedSkillForDetail, setSelectedSkillForDetail] = useState<SkillInfo | null>(null);
  const [readmeEditorTab, setReadmeEditorTab] = useState<'edit' | 'preview'>('edit');
  const [detailActiveTab, setDetailActiveTab] = useState('readme');
  const handleDetailTabClick = useTabRefresh(detailActiveTab, (_key) => {
    if (detailActiveTab === 'readme' && readmeName) {
      setReadmeLoading(true);
      skillsApi.readme(readmeName)
        .then((fresh) => {
          setReadmeContent(fresh);
          setReadmeEditValue(fresh);
        })
        .catch(() => {
          setReadmeContent(`# ${readmeName}\n\n${t('skills.readmeNotFound')}`);
          setReadmeEditValue(`# ${readmeName}\n\n${t('skills.readmeNotFound')}`);
        })
        .finally(() => {
          setReadmeLoading(false);
        });
    }
    if (detailActiveTab === 'commands' && selectedSkillForDetail?.name) {
      qc.invalidateQueries({ queryKey: ['skills', selectedSkillForDetail.name, 'commands'] });
    }
  });

  // ── TanStack Query ────────────────────────────────────────────────────────
  const { data: listData, isLoading, isError, refetch, isRefetching } = useQuery({
    queryKey: queryKeys.skills.list(),
    queryFn: () => skillsApi.list({ per_page: 100 }),
    staleTime: 30_000,
  });
  const githubTokenStatusQ = useQuery({
    queryKey: [...queryKeys.skills.all, 'github-token-status'],
    queryFn: skillsApi.githubTokenStatus,
    staleTime: 60_000,
  });

  const marketReposQ = useInfiniteQuery({
    queryKey: [...queryKeys.skills.marketReposRoot(), { per_page: REPO_PAGE_SIZE }],
    initialPageParam: 1,
    queryFn: ({ pageParam }) =>
      skillsApi.listMarketRepositories({
        page: Number(pageParam) || 1,
        per_page: REPO_PAGE_SIZE,
      }),
    enabled: activeView === 'market' || activeView === 'repositories',
    staleTime: 30_000,
    getNextPageParam: (lastPage, pages) => {
      if (lastPage.hasMore) return (lastPage.page ?? pages.length) + 1;
      const loaded = pages.reduce((sum, page) => sum + (page.items?.length ?? 0), 0);
      const total = Number(lastPage.total ?? loaded);
      return loaded < total ? pages.length + 1 : undefined;
    },
  });

  const marketSearchQ = useInfiniteQuery({
    queryKey: [...queryKeys.skills.marketSearchRoot(), { q: marketKeyword, per_page: MARKET_PAGE_SIZE }],
    initialPageParam: 1,
    queryFn: ({ pageParam }) =>
      skillsApi.searchMarketSkills({
        q: marketKeyword || undefined,
        page: Number(pageParam) || 1,
        per_page: MARKET_PAGE_SIZE,
      }),
    enabled: activeView === 'market',
    staleTime: 20_000,
    getNextPageParam: (lastPage, pages) => {
      if (lastPage.hasMore) return (lastPage.page ?? pages.length) + 1;
      const loaded = pages.reduce((sum, page) => sum + (page.items?.length ?? 0), 0);
      const total = Number(lastPage.total ?? loaded);
      return loaded < total ? pages.length + 1 : undefined;
    },
  });

  const refetchMarketRepos = marketReposQ.refetch;
  const refetchMarketSearch = marketSearchQ.refetch;
  const marketReposLoading = marketReposQ.isLoading;
  const marketSearchLoading = marketSearchQ.isLoading;

  const skills: SkillInfo[] = listData?.skills ?? [];
  const marketRepos = useMemo(
    () => marketReposQ.data?.pages.flatMap((page) => page.items ?? []) ?? [],
    [marketReposQ.data],
  );
  const marketResults = useMemo(
    () => marketSearchQ.data?.pages.flatMap((page) => page.items ?? []) ?? [],
    [marketSearchQ.data],
  );
  const marketSearchTotal = marketSearchQ.data?.pages[0]?.total ?? marketResults.length;
  const marketReposHasMore = Boolean(marketReposQ.hasNextPage);
  const marketSearchHasMore = Boolean(marketSearchQ.hasNextPage);
  const marketReposLoadingMore = marketReposQ.isFetchingNextPage;
  const marketSearchLoadingMore = marketSearchQ.isFetchingNextPage;
  const fetchNextMarketReposPage = marketReposQ.fetchNextPage;
  const fetchNextMarketSearchPage = marketSearchQ.fetchNextPage;

  const marketItemKey = useCallback(
    (repoFullName?: string | null, branch?: string | null, skillPath?: string | null) =>
      `${(repoFullName ?? '').trim().toLowerCase()}@${(branch ?? '').trim().toLowerCase()}:${(skillPath ?? '').trim().replace(/^\/+|\/+$/g, '').toLowerCase()}`,
    [],
  );

  const installedMarketItemHintSet = useMemo(
    () => new Set(installedMarketItemHints),
    [installedMarketItemHints],
  );

  const installedMarketItemIdsFromSkills = useMemo(() => {
    const ids = new Set<string>();
    const installedOriginKeys = new Set(
      skills
        .filter((item) => item.source === 'marketplace' && item.marketplaceOrigin)
        .map((item) =>
          marketItemKey(
            item.marketplaceOrigin?.repoFullName,
            item.marketplaceOrigin?.branch,
            item.marketplaceOrigin?.skillPath,
          ),
        ),
    );
    for (const item of marketResults) {
      if (installedOriginKeys.has(marketItemKey(item.repoFullName, item.branch, item.skillPath))) {
        ids.add(item.id);
      }
    }
    return ids;
  }, [marketItemKey, marketResults, skills]);

  const isMarketItemInstalled = useCallback((item: SkillMarketSearchItem) => {
    return (
      installedMarketItemHintSet.has(item.id) ||
      installedMarketItemIdsFromSkills.has(item.id)
    );
  }, [installedMarketItemHintSet, installedMarketItemIdsFromSkills]);

  // ── Derived stats ─────────────────────────────────────────────────────────
  const stats = useMemo(() => {
    const enabled = skills.filter((s) => s.enabled);
    const uploaded = skills.filter((s) => s.source === 'uploaded');
    return {
      total: skills.length,
      enabled: enabled.length,
      disabled: skills.length - enabled.length,
      uploaded: uploaded.length,
    };
  }, [skills]);

  // ── Filtered list ─────────────────────────────────────────────────────────
  const filtered = useMemo(() => {
    const kw = keyword.toLowerCase();
    return skills.filter((s) => {
      if (kw && !s.name.toLowerCase().includes(kw)
          && !(s.description ?? '').toLowerCase().includes(kw)
          && !s.tags.some((t) => t.toLowerCase().includes(kw))) return false;
      if (sourceFilter && s.source !== sourceFilter) return false;
      if (statusFilter !== undefined && s.enabled !== statusFilter) return false;
      return true;
    });
  }, [skills, keyword, sourceFilter, statusFilter]);

  // ── Mutations ─────────────────────────────────────────────────────────────
  const toggleMut = useMutation({
    mutationFn: ({ name, enabled }: { name: string; enabled: boolean }) =>
      skillsApi.toggle(name, enabled),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.all });
      message.success(t('skills.toggleSuccess'));
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('common.operateFailed'));
    },
  });

  const deleteMut = useMutation({
    mutationFn: ({ name, permanentlyDelete }: { name: string; permanentlyDelete?: boolean }) =>
      skillsApi.delete(name, permanentlyDelete),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.all });
      message.success(t('skills.deleteSuccess'));
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('common.operateFailed'));
    },
  });

  const updateMut = useMutation({
    mutationFn: ({ name, data }: { name: string; data: { description?: string; tags?: string[] } }) =>
      skillsApi.update(name, data),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.all });
      message.success(t('skills.updateSuccess'));
      setEditModalOpen(false);
      setEditingSkill(null);
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('common.operateFailed'));
    },
  });

  const saveReadmeMut = useMutation({
    mutationFn: ({ name, content }: { name: string; content: string }) =>
      skillsApi.saveReadme({ name, content }),
    onSuccess: async (_, { name, content }) => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.all });
      // Re-read from the server to confirm the file was actually written.
      // The backend hot-reloads skill instructions after saving, so a fresh
      // GET is the authoritative source of truth.
      try {
        const fresh = await skillsApi.readme(name);
        setReadmeContent(fresh);
        setReadmeEditValue(fresh);
      } catch {
        // Fall back to local content if re-read fails (e.g. concurrent edits).
        setReadmeContent(content);
        setReadmeEditValue(content);
      }
      setReadmeEditMode(false);
      message.success(t('skills.readmeSaveSuccess'));
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('skills.readmeSaveFailed'));
    },
  });

  const addMarketRepoMut = useMutation({
    mutationFn: (payload: { repoUrl: string; branch?: string }) => skillsApi.addMarketRepository(payload),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.marketReposRoot() });
      qc.invalidateQueries({ queryKey: queryKeys.skills.marketSearchRoot() });
      message.success(t('skills.repoAddSuccess', '仓库添加成功'));
      setRepoUrlInput('');
      setRepoBranchInput('main');
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('skills.repoAddFailed', '仓库添加失败'));
    },
  });

  const deleteMarketRepoMut = useMutation({
    mutationFn: (id: string) => skillsApi.deleteMarketRepository(id),
    onMutate: (id) => {
      setDeletingRepoIds((prev) => (prev.includes(id) ? prev : [...prev, id]));
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.marketReposRoot() });
      qc.invalidateQueries({ queryKey: queryKeys.skills.marketSearchRoot() });
      message.success(t('skills.repoDeleteSuccess', '仓库删除成功'));
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('skills.repoDeleteFailed', '仓库删除失败'));
    },
    onSettled: (_data, _error, id) => {
      setDeletingRepoIds((prev) => prev.filter((x) => x !== id));
    },
  });

  const scanMarketRepoMut = useMutation({
    mutationFn: (id: string) => skillsApi.scanMarketRepository(id),
    onMutate: (id) => {
      setScanningRepoIds((prev) => (prev.includes(id) ? prev : [...prev, id]));
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.marketReposRoot() });
      qc.invalidateQueries({ queryKey: queryKeys.skills.marketSearchRoot() });
      message.success(t('skills.repoScanSuccess', '仓库扫描完成'));
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('skills.repoScanFailed', '仓库扫描失败'));
    },
    onSettled: (_data, _error, id) => {
      setScanningRepoIds((prev) => prev.filter((x) => x !== id));
    },
  });

  const installMarketSkillMut = useMutation({
    mutationFn: (payload: {
      marketItemId?: string;
      repoFullName: string;
      repoUrl?: string;
      branch: string;
      skillPath: string;
      installName?: string;
    }) => {
      const { marketItemId: _marketItemId, ...requestPayload } = payload;
      return skillsApi.installMarketSkill(requestPayload);
    },
    onMutate: (payload) => {
      if (payload.marketItemId) {
        setInstallingMarketItemId(payload.marketItemId);
      }
    },
    onSuccess: (result, payload) => {
      qc.invalidateQueries({ queryKey: queryKeys.skills.all });
      setInstalledMarketItemHints((prev) => {
        const next = new Set(prev);
        if (payload.marketItemId) next.add(payload.marketItemId);
        if (result?.installedFrom?.id) next.add(result.installedFrom.id);
        return Array.from(next);
      });
      message.success(t('skills.marketInstallSuccess', 'Skill 安装成功'));
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('skills.marketInstallFailed', 'Skill 安装失败'));
    },
    onSettled: () => {
      setInstallingMarketItemId(null);
    },
  });

  // ── Handlers ──────────────────────────────────────────────────────────────
  const handleViewReadme = useCallback(async (skill: SkillInfo) => {
    setSelectedSkillForDetail(skill);
    setReadmeName(skill.name);
    setReadmeContent(null);
    setReadmeEditMode(false);
    setReadmeDrawerOpen(true);
    setReadmeLoading(true);
    try {
      const content = await skillsApi.readme(skill.name);
      setReadmeContent(content);
      setReadmeEditValue(content);
    } catch {
      setReadmeContent(`# ${skill.name}\n\n${t('skills.readmeNotFound')}`);
      setReadmeEditValue(`# ${skill.name}\n\n${t('skills.readmeNotFound')}`);
    } finally {
      setReadmeLoading(false);
    }
  }, [t]);

  const handleOpenUpload = () => {
    setUploadForm({ name: '', description: '', tags: [] });
    setSelectedZip(null);
    setZipSecurityScan(null);
    setRiskConfirmed(false);
    setUploadModalOpen(true);
  };

  const handleMarketSearch = () => {
    const nextKeyword = marketSearchInput.trim();
    if (nextKeyword === marketKeyword) {
      void marketSearchQ.refetch();
      return;
    }
    setMarketKeyword(nextKeyword);
  };

  const handleMarketListScroll = useCallback((event: React.UIEvent<HTMLDivElement>) => {
    if (marketSearchLoadingMore || !marketSearchHasMore) return;
    const target = event.currentTarget;
    const nearBottom = target.scrollTop + target.clientHeight >= target.scrollHeight - 24;
    if (nearBottom) {
      void fetchNextMarketSearchPage();
    }
  }, [fetchNextMarketSearchPage, marketSearchHasMore, marketSearchLoadingMore]);

  const handleRepoListScroll = useCallback((event: React.UIEvent<HTMLDivElement>) => {
    if (marketReposLoadingMore || !marketReposHasMore) return;
    const target = event.currentTarget;
    const nearBottom = target.scrollTop + target.clientHeight >= target.scrollHeight - 24;
    if (nearBottom) {
      void fetchNextMarketReposPage();
    }
  }, [fetchNextMarketReposPage, marketReposHasMore, marketReposLoadingMore]);

  useEffect(() => {
    if (activeView === 'market' && marketScrollRef.current) {
      marketScrollRef.current.scrollTop = 0;
    }
  }, [activeView, marketKeyword]);

  useEffect(() => {
    if (activeView === 'repositories' && repoScrollRef.current) {
      repoScrollRef.current.scrollTop = 0;
    }
  }, [activeView]);

  useEffect(() => {
    if (activeView !== 'market') return;
    const container = marketScrollRef.current;
    if (!container || marketSearchLoadingMore || !marketSearchHasMore) return;
    if (container.scrollHeight <= container.clientHeight + 8) {
      void fetchNextMarketSearchPage();
    }
  }, [
    activeView,
    fetchNextMarketSearchPage,
    marketResults.length,
    marketSearchHasMore,
    marketSearchLoadingMore,
  ]);

  useEffect(() => {
    if (activeView !== 'repositories') return;
    const container = repoScrollRef.current;
    if (!container || marketReposLoadingMore || !marketReposHasMore) return;
    if (container.scrollHeight <= container.clientHeight + 8) {
      void fetchNextMarketReposPage();
    }
  }, [
    activeView,
    fetchNextMarketReposPage,
    marketRepos.length,
    marketReposHasMore,
    marketReposLoadingMore,
  ]);

  const handleZipFileSelected = async (file: File) => {
    if (!file.name.toLowerCase().endsWith('.zip')) {
      message.error(t('skills.zipFileFormat'));
      return false;
    }
    setSelectedZip(file);
    setZipSecurityScan(null);
    setRiskConfirmed(false);
    setZipScanning(true);

    try {
      const result = await skillsApi.previewZip(file);
      setZipSecurityScan(result.securityScan);
      setUploadForm((prev) => ({
        name: prev.name.trim() || result.name || prev.name,
        description: prev.description.trim() || result.description || prev.description,
        tags: prev.tags.length > 0 ? prev.tags : (result.tags?.length > 0 ? result.tags : prev.tags),
      }));
    } catch (err: unknown) {
      setSelectedZip(null);
      if (err instanceof ApiError && err.code === 'ECONNABORTED') message.error(t('skills.scanTimedOut'));
      else if (err instanceof ApiError) message.error(err.message);
      else message.error(t('skills.scanFailed'));
    } finally {
      setZipScanning(false);
    }

    return false;
  };

  const handleUploadSubmit = async () => {
    // name is the only truly required field — allow submission even if empty
    // so the backend can derive it from the zip.
    if (!selectedZip) {
      message.error(t('skills.selectZipFirst'));
      return;
    }
    if (!zipSecurityScan || zipScanning) {
      message.warning(t('skills.scanPending'));
      return;
    }
    if (zipSecurityScan.requiresConfirmation && !riskConfirmed) {
      message.warning(t('skills.confirmRiskRequired'));
      return;
    }

    setUploading(true);
    try {
      const result = await skillsApi.uploadZip(
        selectedZip,
        uploadForm.name.trim() || undefined,
        uploadForm.description.trim() || undefined,
        uploadForm.tags.length > 0 ? uploadForm.tags : undefined,
        riskConfirmed,
      );
      qc.invalidateQueries({ queryKey: queryKeys.skills.all });
      if (result.warnings.length > 0) {
        message.warning(`${t('skills.uploadSuccess')} — ${t('skills.riskyWarning')}: ${result.warnings.join(', ')}`);
      } else {
        message.success(t('skills.uploadSuccess'));
      }
      setUploadModalOpen(false);
      setUploadForm({ name: '', description: '', tags: [] });
      setSelectedZip(null);
      setZipSecurityScan(null);
      setRiskConfirmed(false);
    } catch (err: unknown) {
      if (err instanceof ApiError) message.error(err.message);
      else message.error(t('common.operateFailed'));
    } finally {
      setUploading(false);
    }
  };

  // ── Table columns ────────────────────────────────────────────────────────
  const columns: ColumnsType<SkillInfo> = [
    {
      title: t('skills.columns.name'),
      dataIndex: 'name',
      key: 'name',
      width: 200,
      render: (name: string, r) => (
        <Space>
          <Text strong style={{ fontFamily: 'monospace', fontSize: 13 }}>{name}</Text>
          {!r.enabled && <Tag color="default">{t('common.disabled')}</Tag>}
        </Space>
      ),
    },
    {
      title: t('skills.columns.source'),
      dataIndex: 'source',
      key: 'source',
      width: 90,
      render: (src: string) => (
        <Tag color={SOURCE_COLORS[src] ?? 'default'}>
          {t(`skills.source.${src}`, { defaultValue: src })}
        </Tag>
      ),
    },
    {
      title: t('skills.columns.description'),
      dataIndex: 'description',
      key: 'description',
      ellipsis: { showTitle: false },
      render: (desc: string | undefined) => (
        <Tooltip title={desc || t('common.noData')}>
          <Text type="secondary" style={{ fontSize: 13 }}>
            {desc || t('common.noData')}
          </Text>
        </Tooltip>
      ),
    },
    {
      title: t('skills.columns.tags'),
      dataIndex: 'tags',
      key: 'tags',
      width: 160,
      render: (tags: string[]) =>
        tags?.length > 0 ? (
          tags.slice(0, 3).map((tag) => (
            <Tag key={tag} color="purple" style={{ margin: 1 }}>{tag}</Tag>
          ))
        ) : (
          <Text type="secondary" style={{ fontSize: 12 }}>—</Text>
        ),
    },
    {
      title: t('skills.columns.version'),
      dataIndex: 'version',
      key: 'version',
      width: 80,
      render: (v: string) => <Text code style={{ fontSize: 11 }}>{v}</Text>,
    },
    {
      title: t('skills.columns.updatedAt'),
      dataIndex: 'updated_at',
      key: 'updated_at',
      width: 120,
      render: (ts: string) => (
        <Text type="secondary" style={{ fontSize: 12 }}>
          {dayjs(ts).fromNow()}
        </Text>
      ),
    },
    {
      title: t('common.actions'),
      key: 'action',
      width: 180,
      fixed: 'right',
      render: (_, r) => {
        const rowToggleLoading =
          toggleMut.isPending && toggleMut.variables?.name === r.name;
        const rowDeleteLoading =
          deleteMut.isPending && deleteMut.variables?.name === r.name;
        return (
          <Space size={4}>
            <Tooltip title={t('skills.viewReadme')}>
              <Button
                size="small"
                icon={<EyeOutlined />}
                onClick={() => handleViewReadme(r)}
              />
            </Tooltip>
            <Tooltip title={!canWrite ? t('common.noPermission') : undefined}>
              <span>
                <Switch
                  size="small"
                  checked={r.enabled}
                  disabled={!canWrite}
                  loading={rowToggleLoading}
                  onChange={(checked) => toggleMut.mutate({ name: r.name, enabled: checked })}
                />
              </span>
            </Tooltip>
            {canWrite && (
              <Tooltip title={t('common.edit')}>
                <Button
                  size="small"
                  icon={<EditOutlined />}
                  onClick={() => {
                    setEditingSkill(r);
                    setEditForm({ name: r.name, description: r.description ?? '', tags: r.tags ?? [] });
                    setEditModalOpen(true);
                  }}
                />
              </Tooltip>
            )}
            {canDelete && (
              <Popconfirm
                title={t('skills.deleteConfirm')}
                description={t('skills.deletePermanentDescription')}
                onConfirm={() => deleteMut.mutate({ name: r.name, permanentlyDelete: true })}
                okText={t('common.confirm')}
                cancelText={t('common.cancel')}
                okButtonProps={{ danger: true, loading: rowDeleteLoading }}
              >
                <Tooltip title={t('common.delete')}>
                  <Button
                    size="small"
                    danger
                    icon={<DeleteOutlined />}
                    loading={rowDeleteLoading}
                  />
                </Tooltip>
              </Popconfirm>
            )}
          </Space>
        );
      },
    },
  ];

  const marketRepoColumns: ColumnsType<SkillMarketRepository> = [
    {
      title: t('skills.repoColumns.repo', '仓库'),
      dataIndex: 'repoFullName',
      key: 'repoFullName',
      render: (v: string, row) => (
        <Space direction="vertical" size={0}>
          <Text strong>{v}</Text>
          <Text type="secondary">{t('skills.repoColumns.branch', '分支')}: {row.branch}</Text>
        </Space>
      ),
    },
    {
      title: t('skills.repoColumns.discovered', 'Skills 数'),
      dataIndex: 'discoveredCount',
      key: 'discoveredCount',
      width: 130,
      render: (v: number) => <Tag>{v}</Tag>,
    },
    {
      title: t('common.status'),
      dataIndex: 'lastScanStatus',
      key: 'lastScanStatus',
      width: 140,
      render: (v: string, row) => {
        if (v === 'failed') {
          return <Tooltip title={row.lastScanError || '-'}><Tag color="red">{t('skills.scanStatusFailed', '失败')}</Tag></Tooltip>;
        }
        if (v === 'success') return <Tag color="green">{t('skills.scanSuccessTag', '已扫描')}</Tag>;
        return <Tag>{t('skills.scanIdle', '未扫描')}</Tag>;
      },
    },
    {
      title: t('common.actions'),
      key: 'action',
      width: 220,
      render: (_: unknown, row) => {
        const rowScanLoading = scanningRepoIds.includes(row.id);
        const rowDeleteLoading = deletingRepoIds.includes(row.id);
        return (
          <Space>
            <Tooltip title={t('skills.repoRescan', '重新扫描')}>
              <Button
                size="small"
                icon={<SyncOutlined />}
                disabled={!canWrite}
                loading={rowScanLoading}
                onClick={() => scanMarketRepoMut.mutate(row.id)}
              />
            </Tooltip>
            <Tooltip title={t('skills.repoOpen', '打开仓库')}>
              <Button
                size="small"
                icon={<LinkOutlined />}
                onClick={() => window.open(row.repoUrl, '_blank', 'noopener,noreferrer')}
              />
            </Tooltip>
            {!row.builtIn && canDelete && (
              <Popconfirm
                title={t('skills.repoDeleteConfirm', '确认删除该仓库？')}
                onConfirm={() => deleteMarketRepoMut.mutate(row.id)}
                okText={t('common.confirm')}
                cancelText={t('common.cancel')}
              >
                <Button
                  size="small"
                  danger
                  icon={<DeleteOutlined />}
                  loading={rowDeleteLoading}
                />
              </Popconfirm>
            )}
          </Space>
        );
      },
    },
  ];

  const marketSearchColumns: ColumnsType<SkillMarketSearchItem> = [
    {
      title: t('skills.marketColumns.skill', 'Skill'),
      dataIndex: 'skillName',
      key: 'skillName',
      render: (v: string, row) => (
        <Space direction="vertical" size={0}>
          <Text strong>{v}</Text>
          <Text type="secondary" style={{ fontSize: 12 }}>{row.skillPath}</Text>
        </Space>
      ),
    },
    {
      title: t('skills.marketColumns.repo', '仓库'),
      dataIndex: 'repoFullName',
      key: 'repoFullName',
      width: 280,
      render: (v: string, row) => (
        <Space direction="vertical" size={0}>
          <Text>{v}</Text>
          <Text type="secondary" style={{ fontSize: 12 }}>{row.branch}</Text>
        </Space>
      ),
    },
    {
      title: t('common.actions'),
      key: 'action',
      width: 220,
      render: (_: unknown, row) => {
        const installed = isMarketItemInstalled(row);
        const rowLoading =
          installMarketSkillMut.isPending && installingMarketItemId === row.id;
        return (
          <Space>
            <Tooltip title={t('skills.viewReadme', '查看 README')}>
              <Button
                size="small"
                icon={<EyeOutlined />}
                onClick={() => {
                  if (row.htmlUrl) window.open(row.htmlUrl, '_blank', 'noopener,noreferrer');
                }}
              />
            </Tooltip>
            {installed ? (
              <Button
                size="small"
                icon={<CheckCircleOutlined />}
                disabled
              >
                {t('skills.marketInstalled', '已安装')}
              </Button>
            ) : (
              <Button
                type="primary"
                size="small"
                icon={<ThunderboltOutlined />}
                disabled={!canWrite}
                loading={rowLoading}
                onClick={() =>
                  installMarketSkillMut.mutate({
                    marketItemId: row.id,
                    repoFullName: row.repoFullName,
                    repoUrl: row.repoUrl,
                    branch: row.branch,
                    skillPath: row.skillPath,
                  })
                }
              >
                {t('skills.marketInstall', '一键安装')}
              </Button>
            )}
          </Space>
        );
      },
    },
  ];

  if (isLoading) return <PageSkeleton rows={6} />;

  return (
    <div style={{ padding: '24px 24px 0' }}>
      {/* ── Header ──────────────────────────────────────────────────────── */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: 20, gap: 16 }}>
        <div>
          <Title level={3} style={{ margin: 0 }}>
            <StarOutlined style={{ marginRight: 8 }} />
            {t('skills.title')}
          </Title>
          <Paragraph type="secondary" style={{ margin: '4px 0 0', fontSize: 13 }}>
            {t('skills.subtitle')}
          </Paragraph>
        </div>
        <Space>
          {connected && (
            <Tag color="green" icon={<ReloadOutlined />} style={{ alignSelf: 'center' }}>
              {t('skills.hotReload')}
            </Tag>
          )}
          <Button
            icon={<ReloadOutlined spin={isRefetching || marketReposQ.isFetching || marketSearchQ.isFetching} />}
            onClick={() => {
              void refetch();
              void refetchMarketRepos();
              void refetchMarketSearch();
            }}
          >
            {t('common.refresh')}
          </Button>
          {canWrite && activeView === 'installed' && (
            <Button type="primary" icon={<PlusOutlined />} onClick={handleOpenUpload}>
              {t('skills.add')}
            </Button>
          )}
        </Space>
      </div>
      {githubTokenStatusQ.data?.configured === false && (
        <Alert
          showIcon
          type="warning"
          style={{ marginBottom: 16 }}
          message={t('skills.githubTokenMissing')}
          description={t('skills.githubTokenMissingHelp')}
          action={(
            <Button
              type="link"
              icon={<SettingOutlined />}
              onClick={() => navigate('/config/management')}
            >
              {t('skills.githubTokenConfigure')}
            </Button>
          )}
        />
      )}

      <Card styles={{ body: { padding: '12px 16px' } }} style={{ marginBottom: 16 }}>
        <Segmented
          block
          value={activeView}
          onChange={(value) => setActiveView(value as 'installed' | 'market' | 'repositories')}
          options={[
            { label: t('skills.tabInstalled', '已安装技能'), value: 'installed' },
            { label: t('skills.tabMarket', '市场搜索'), value: 'market' },
            { label: t('skills.tabRepositories', '仓库管理'), value: 'repositories' },
          ]}
        />
      </Card>

      {activeView === 'installed' && (
        <>
          {/* ── Stats ───────────────────────────────────────────────────────── */}
          <Row gutter={16} style={{ marginBottom: 20 }}>
            <Col span={6}>
              <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
                <Statistic title={t('skills.statTotal')} value={stats.total} valueStyle={{ fontSize: 28, fontWeight: 600 }} />
              </Card>
            </Col>
            <Col span={6}>
              <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
                <Statistic title={t('skills.statEnabled')} value={stats.enabled} valueStyle={{ fontSize: 28, fontWeight: 600, color: '#3fb950' }} />
              </Card>
            </Col>
            <Col span={6}>
              <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
                <Statistic title={t('skills.statDisabled')} value={stats.disabled} valueStyle={{ fontSize: 28, fontWeight: 600, color: '#8b949e' }} />
              </Card>
            </Col>
            <Col span={6}>
              <Card size="small" styles={{ body: { padding: '12px 16px' } }}>
                <Statistic title={t('skills.statUploaded')} value={stats.uploaded} valueStyle={{ fontSize: 28, fontWeight: 600, color: '#1677ff' }} />
              </Card>
            </Col>
          </Row>

          {/* ── Filters ─────────────────────────────────────────────────────── */}
          <Card styles={{ body: { padding: '12px 16px', marginBottom: 16 } }}>
            <Space wrap size={12}>
              <Input
                placeholder={t('skills.searchPlaceholder')}
                prefix={<SearchOutlined />}
                style={{ width: 240 }}
                value={keyword}
                onChange={(e) => setKeyword(e.target.value)}
                allowClear
              />
              <Select
                placeholder={t('skills.filterBySource')}
                allowClear
                style={{ width: 140 }}
                value={sourceFilter}
                onChange={setSourceFilter}
                options={[
                  { value: 'uploaded', label: t('skills.source.uploaded') },
                  { value: 'builtin', label: t('skills.source.builtin') },
                ]}
              />
              <Select
                placeholder={t('skills.filterByStatus')}
                allowClear
                style={{ width: 140 }}
                value={statusFilter}
                onChange={setStatusFilter}
                options={[
                  { value: true, label: t('common.enabled') },
                  { value: false, label: t('common.disabled') },
                ]}
              />
              {(keyword || sourceFilter || statusFilter !== undefined) && (
                <Button size="small" onClick={() => { setKeyword(''); setSourceFilter(undefined); setStatusFilter(undefined); }}>
                  {t('common.clearFilters')}
                </Button>
              )}
            </Space>
          </Card>

          {/* ── Table ───────────────────────────────────────────────────────── */}
          <Card styles={{ body: { padding: 0 } }}>
            <Table
              columns={columns}
              dataSource={filtered}
              rowKey="id"
              pagination={{ pageSize: 20, size: 'small', showTotal: () => `共 ${filtered.length} 个 Skill` }}
              scroll={{ x: 900 }}
              locale={{
                emptyText: (
                  <div style={{ padding: '48px 0', textAlign: 'center' }}>
                    <Text type="secondary" style={{ fontSize: 14 }}>
                      {isError ? t('common.loadFailed') : t('skills.empty.title')}
                    </Text>
                    <br />
                    {isError ? (
                      <Button type="primary" onClick={() => refetch()} style={{ marginTop: 16 }}>
                        {t('common.retry')}
                      </Button>
                    ) : canWrite ? (
                      <Button type="primary" icon={<UploadOutlined />} onClick={handleOpenUpload} style={{ marginTop: 16 }}>
                        {t('skills.uploadFirst')}
                      </Button>
                    ) : (
                      <Text type="secondary" style={{ fontSize: 13 }}>
                        {t('skills.empty.description')}
                      </Text>
                    )}
                  </div>
                ),
              }}
            />
          </Card>
        </>
      )}

      {activeView === 'market' && (
        <>
          <Card styles={{ body: { padding: '12px 16px', marginBottom: 16 } }}>
            <Space wrap>
              <Input
                placeholder={t('skills.marketSearchPlaceholder', '输入关键词搜索 Skill（名称/路径/仓库）')}
                prefix={<SearchOutlined />}
                style={{ width: 420 }}
                value={marketSearchInput}
                onChange={(e) => setMarketSearchInput(e.target.value)}
                onPressEnter={handleMarketSearch}
                allowClear
              />
              <Button
                type="primary"
                icon={<SearchOutlined />}
                loading={marketSearchLoading}
                onClick={handleMarketSearch}
              >
                {t('skills.marketSearchAction', '搜索')}
              </Button>
              <Text type="secondary">
                {t('skills.marketSearchResultCount', '共 {{count}} 条', { count: marketSearchTotal })}
              </Text>
            </Space>
          </Card>
          <Card styles={{ body: { padding: 0 } }}>
            <div
              ref={marketScrollRef}
              style={{ maxHeight: 560, overflowY: 'auto' }}
              onScroll={handleMarketListScroll}
            >
              <Table
                columns={marketSearchColumns}
                dataSource={marketResults}
                rowKey="id"
                loading={marketSearchLoading}
                pagination={false}
                locale={{ emptyText: t('skills.marketEmpty', '暂无可安装 Skill，请先在仓库管理中添加仓库并扫描。') }}
              />
            </div>
            {marketResults.length > 0 && (
              <div style={{ padding: '8px 16px' }}>
                {marketSearchLoadingMore ? (
                  <Text type="secondary">{t('common.loading', '加载中...')}</Text>
                ) : marketSearchHasMore ? (
                  <Text type="secondary">{t('skills.scrollToLoadMore', '下滑可加载更多')}</Text>
                ) : (
                  <Text type="secondary">{t('skills.allLoaded', '已加载全部')}</Text>
                )}
              </div>
            )}
          </Card>
        </>
      )}

      {activeView === 'repositories' && (
        <>
          <Card styles={{ body: { padding: '12px 16px', marginBottom: 16 } }}>
            <Space wrap>
              <Input
                placeholder={t('skills.repoUrlPlaceholder', 'owner/repo 或 https://github.com/owner/repo')}
                style={{ width: 420 }}
                value={repoUrlInput}
                onChange={(e) => setRepoUrlInput(e.target.value)}
              />
              <Input
                placeholder={t('skills.repoBranchPlaceholder', '分支')}
                style={{ width: 140 }}
                value={repoBranchInput}
                onChange={(e) => setRepoBranchInput(e.target.value)}
              />
              <Button
                type="primary"
                icon={<PlusOutlined />}
                disabled={!canWrite}
                loading={addMarketRepoMut.isPending}
                onClick={() => {
                  if (!repoUrlInput.trim()) {
                    message.warning(t('skills.repoUrlRequired', '请先输入仓库 URL'));
                    return;
                  }
                  addMarketRepoMut.mutate({
                    repoUrl: repoUrlInput.trim(),
                    branch: repoBranchInput.trim() || 'main',
                  });
                }}
              >
                {t('skills.repoAddAction', '添加仓库')}
              </Button>
            </Space>
          </Card>
          <Card styles={{ body: { padding: 0 } }}>
            <div
              ref={repoScrollRef}
              style={{ maxHeight: 560, overflowY: 'auto' }}
              onScroll={handleRepoListScroll}
            >
              <Table
                columns={marketRepoColumns}
                dataSource={marketRepos}
                rowKey="id"
                loading={marketReposLoading}
                pagination={false}
                locale={{ emptyText: t('skills.repoEmpty', '暂无仓库，请先添加。') }}
              />
            </div>
            {marketRepos.length > 0 && (
              <div style={{ padding: '8px 16px' }}>
                {marketReposLoadingMore ? (
                  <Text type="secondary">{t('common.loading', '加载中...')}</Text>
                ) : marketReposHasMore ? (
                  <Text type="secondary">{t('skills.scrollToLoadMore', '下滑可加载更多')}</Text>
                ) : (
                  <Text type="secondary">{t('skills.allLoaded', '已加载全部')}</Text>
                )}
              </div>
            )}
          </Card>
        </>
      )}

      {/* ── Skill Detail Drawer ──────────────────────────────────────────── */}
      <Drawer
        title={
          <Space>
            <StarOutlined />
            <Text strong>{selectedSkillForDetail?.name}</Text>
            {selectedSkillForDetail && (
              <Tag color={SOURCE_COLORS[selectedSkillForDetail.source] ?? 'default'}>
                {selectedSkillForDetail.source}
              </Tag>
            )}
          </Space>
        }
        open={readmeDrawerOpen}
        onClose={() => { setReadmeDrawerOpen(false); setReadmeEditMode(false); }}
        width={720}
        destroyOnHidden
      >
        {selectedSkillForDetail && (
          <>
            {/* Skill metadata summary */}
            <Spin spinning={readmeLoading} tip={t('common.loading')}>
            <Descriptions size="small" column={2} style={{ marginBottom: 16 }}>
              <Descriptions.Item label={t('skills.columns.version')}>
                <Text code>{selectedSkillForDetail.version}</Text>
              </Descriptions.Item>
              <Descriptions.Item label={t('common.status')}>
                <Tag color={selectedSkillForDetail.enabled ? 'success' : 'default'}>
                  {selectedSkillForDetail.enabled ? t('common.enabled') : t('common.disabled')}
                </Tag>
              </Descriptions.Item>
              <Descriptions.Item label={t('skills.columns.commandsCount')}>
                <Tag icon={<CodeOutlined />}>{selectedSkillForDetail.commands_count}</Tag>
              </Descriptions.Item>
              <Descriptions.Item label={t('common.createdAt')}>
                {dayjs(selectedSkillForDetail.created_at).format('YYYY-MM-DD HH:mm')}
              </Descriptions.Item>
              {selectedSkillForDetail.description && (
                <Descriptions.Item label={t('skills.columns.description')} span={2}>
                  <Text type="secondary">{selectedSkillForDetail.description}</Text>
                </Descriptions.Item>
              )}
              {selectedSkillForDetail.tags.length > 0 && (
                <Descriptions.Item label={t('skills.columns.tags')} span={2}>
                  {selectedSkillForDetail.tags.map((tag) => (
                    <Tag key={tag} color="purple" style={{ marginRight: 4 }}>{tag}</Tag>
                  ))}
                </Descriptions.Item>
              )}
            </Descriptions>
            </Spin>

            <Divider style={{ margin: '8px 0 12px' }} />

            <Tabs
              activeKey={detailActiveTab}
              onChange={setDetailActiveTab}
              onTabClick={handleDetailTabClick}
              items={[
                {
                  key: 'readme',
                  label: (
                    <Space>
                      <FolderOutlined />
                      {t('skills.readmeTab')}
                    </Space>
                  ),
                  children: (
                    <>
                      <div style={{ marginBottom: 12, display: 'flex', justifyContent: 'flex-end' }}>
                        {canWrite && readmeContent !== null && (
                          readmeEditMode ? (
                            <Space>
                              <Button
                                size="small"
                                icon={<EyeOutlined />}
                                onClick={() => { setReadmeEditMode(false); setReadmeEditValue(readmeContent ?? ''); }}
                              >
                                {t('skills.readmeCancel')}
                              </Button>
                              <Button
                                size="small"
                                type="primary"
                                icon={<SaveOutlined />}
                                loading={readmeSaving}
                                onClick={() => {
                                  setReadmeSaving(true);
                                  saveReadmeMut.mutate(
                                    { name: readmeName, content: readmeEditValue },
                                    { onSettled: () => setReadmeSaving(false) }
                                  );
                                }}
                              >
                                {t('skills.readmeSave')}
                              </Button>
                            </Space>
                          ) : (
                            <Tooltip title={t('skills.readmeEditTooltip')}>
                              <Button
                                size="small"
                                icon={<EditOutlined />}
                                onClick={() => setReadmeEditMode(true)}
                              >
                                {t('skills.readmeEdit')}
                              </Button>
                            </Tooltip>
                          )
                        )}
                      </div>
                      {readmeLoading ? (
                        <div style={{ textAlign: 'center', padding: 40 }}>
                          <Text type="secondary">{t('common.loading')}</Text>
                        </div>
                      ) : (
                        <>
                          <div style={{ marginBottom: 8 }}>
                            <Segmented
                              value={readmeEditorTab}
                              options={[
                                { value: 'edit', label: t('skills.editorTab') },
                                { value: 'preview', label: t('skills.previewTab') },
                              ]}
                              onChange={(v) => setReadmeEditorTab(v as 'edit' | 'preview')}
                            />
                          </div>
                          {readmeEditorTab === 'edit' ? (
                            <MonacoEditor
                              height={400}
                              language="markdown"
                              value={readmeEditValue}
                              onChange={(v) => setReadmeEditValue(v ?? '')}
                              theme="vs-dark"
                              options={{
                                fontSize: 13,
                                fontFamily: "'JetBrains Mono', 'Fira Code', Consolas, monospace",
                                minimap: { enabled: false },
                                lineNumbers: 'on',
                                wordWrap: 'on',
                                scrollBeyondLastLine: false,
                                automaticLayout: true,
                              }}
                            />
                          ) : (
                            <div
                              style={{
                                minHeight: 400,
                                maxHeight: 600,
                                overflow: 'auto',
                                padding: '8px 16px',
                                borderRadius: 8,
                              }}
                            >
                              <Markdown relaxed>{readmeEditValue}</Markdown>
                            </div>
                          )}
                        </>
                      )}
                    </>
                  ),
                },
                {
                  key: 'commands',
                  label: (
                    <Space>
                      <CodeOutlined />
                      {t('skills.commandsTab')}
                      <Tag>{selectedSkillForDetail.commands_count}</Tag>
                    </Space>
                  ),
                  children: (
                    <SkillCommandsTab skillName={selectedSkillForDetail.name} commandsCount={selectedSkillForDetail.commands_count} />
                  ),
                },
              ]}
            />
          </>
        )}
      </Drawer>

      {/* ── Upload Modal ────────────────────────────────────────────────── */}
      <Modal
        title={
          <Space>
            <UploadOutlined />
            <Text strong>{t('skills.uploadModal.title')}</Text>
          </Space>
        }
        open={uploadModalOpen}
        onCancel={() => { setUploadModalOpen(false); setSelectedZip(null); setZipSecurityScan(null); setRiskConfirmed(false); }}
        footer={
          <Space>
            <Button
              type="primary"
              icon={<UploadOutlined />}
              loading={uploading || zipScanning}
              disabled={uploading || zipScanning}
              onClick={handleUploadSubmit}
            >
              {t('skills.uploadZip')}
            </Button>
          </Space>
        }
        width={600}
        destroyOnHidden
      >
        <Space direction="vertical" size={16} style={{ width: '100%' }}>
          {/* Name */}
          <div>
            <Text strong style={{ display: 'block', marginBottom: 6 }}>{t('skills.columns.name')} *</Text>
            <Input
              placeholder={t('skills.namePlaceholder')}
              value={uploadForm.name}
              onChange={(e) => setUploadForm((f) => ({ ...f, name: e.target.value }))}
              status={!uploadForm.name.trim() ? 'error' : undefined}
            />
          </div>

          {/* Description */}
          <div>
            <Text strong style={{ display: 'block', marginBottom: 6 }}>{t('skills.columns.description')}</Text>
            <Input.TextArea
              placeholder={t('skills.descriptionPlaceholder')}
              value={uploadForm.description}
              onChange={(e) => setUploadForm((f) => ({ ...f, description: e.target.value }))}
              rows={2}
            />
          </div>

          {/* Tags */}
          <div>
            <Text strong style={{ display: 'block', marginBottom: 6 }}>{t('skills.columns.tags')}</Text>
            <Select
              mode="tags"
              placeholder={t('skills.tagsPlaceholder')}
              value={uploadForm.tags}
              onChange={(tags) => setUploadForm((f) => ({ ...f, tags }))}
              style={{ width: '100%' }}
              tokenSeparators={[',']}
            />
          </div>

          {/* Zip file */}
          <div>
            <Text strong style={{ display: 'block', marginBottom: 6 }}>{t('skills.uploadZip')} *</Text>
            <Space direction="vertical" style={{ width: '100%' }}>
              <Upload.Dragger
                name="file"
                accept=".zip"
                showUploadList={false}
                beforeUpload={handleZipFileSelected}
                disabled={uploading || zipScanning}
              >
                <p className="ant-upload-drag-icon">
                  {selectedZip
                    ? <FileZipOutlined style={{ fontSize: 28, color: '#1677ff' }} />
                    : <InboxOutlined style={{ fontSize: 28 }} />}
                </p>
                <p className="ant-upload-text">
                  {selectedZip ? selectedZip.name : t('skills.zipDragHint')}
                </p>
                <p className="ant-upload-hint" style={{ fontSize: 12 }}>
                  {t('skills.zipHint')}
                </p>
              </Upload.Dragger>

              {zipScanning && (
                <Alert type="info" showIcon message={t('skills.scanning')} description={t('skills.scanningTokenNotice')} />
              )}
              {zipSecurityScan && zipSecurityScan.findings.length > 0 && (
                <Alert
                  type={zipSecurityScan.status === 'blocked' || zipSecurityScan.findings.some((finding) => finding.severity === 'critical') ? 'error' : 'warning'}
                  showIcon
                  icon={<WarningOutlined />}
                  message={zipSecurityScan.status === 'blocked' || zipSecurityScan.findings.some((finding) => finding.severity === 'critical') ? t('skills.scanCritical') : t('skills.riskyWarning')}
                  description={
                    <Space direction="vertical" size={8} style={{ width: '100%' }}>
                      {zipSecurityScan.findings.map((finding, index) => (
                        <div key={`${finding.source}-${finding.file}-${index}`}>
                          <Space size={4} wrap>
                            <Tag>{t(`skills.scanSource.${finding.source}`)}</Tag>
                            <Tag color={finding.severity === 'critical' ? 'red' : finding.severity === 'high' ? 'orange' : 'gold'}>
                              {t(`skills.scanSeverity.${finding.severity}`)}
                            </Tag>
                            <Text code>{finding.file}</Text>
                          </Space>
                          <div>{finding.evidence}</div>
                          <Text type="secondary">{finding.recommendation}</Text>
                        </div>
                      ))}
                      {zipSecurityScan.requiresConfirmation && (
                        <Checkbox checked={riskConfirmed} onChange={(event) => setRiskConfirmed(event.target.checked)}>
                          {t('skills.confirmRisk')}
                        </Checkbox>
                      )}
                    </Space>
                  }
                />
              )}
              {zipSecurityScan && zipSecurityScan.findings.length === 0 && (
                <Alert
                  type={zipSecurityScan.aiScanned ? 'success' : 'warning'}
                  showIcon
                  icon={<CheckCircleOutlined />}
                  message={zipSecurityScan.aiScanned ? t('skills.scanNoKnownRisk') : t('skills.scanAiUnavailable')}
                  description={zipSecurityScan.aiScanned ? t('skills.scanNoKnownRiskDesc') : t('skills.scanAiUnavailableDesc')}
                />
              )}
            </Space>
          </div>
        </Space>
      </Modal>

      {/* Edit skill modal */}
      <Modal
        title={t('skills.editModalTitle')}
        open={editModalOpen}
        onCancel={() => { setEditModalOpen(false); setEditingSkill(null); }}
        footer={null}
        width={480}
      >
        <Space direction="vertical" size={16} style={{ width: '100%' }}>
          <div>
            <Text strong style={{ display: 'block', marginBottom: 6 }}>{t('skills.columns.name')}</Text>
            <Input value={editForm.name} disabled />
          </div>
          <div>
            <Text strong style={{ display: 'block', marginBottom: 6 }}>{t('skills.columns.description')}</Text>
            <Input.TextArea
              value={editForm.description}
              onChange={(e) => setEditForm((f) => ({ ...f, description: e.target.value }))}
              rows={3}
              placeholder={t('skills.descriptionPlaceholder')}
            />
          </div>
          <div>
            <Text strong style={{ display: 'block', marginBottom: 6 }}>{t('skills.columns.tags')}</Text>
            <Select
              mode="tags"
              value={editForm.tags}
              onChange={(tags) => setEditForm((f) => ({ ...f, tags }))}
              style={{ width: '100%' }}
              tokenSeparators={[',']}
              placeholder={t('skills.tagsPlaceholder')}
            />
          </div>
          <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
            <Button onClick={() => { setEditModalOpen(false); setEditingSkill(null); }}>
              {t('common.cancel')}
            </Button>
            <Button
              type="primary"
              loading={updateMut.isPending}
              onClick={() => {
                if (!editingSkill) return;
                updateMut.mutate({
                  name: editingSkill.name,
                  data: {
                    description: editForm.description || undefined,
                    tags: editForm.tags.length > 0 ? editForm.tags : undefined,
                  },
                });
              }}
            >
              {t('common.save')}
            </Button>
          </Space>
        </Space>
      </Modal>
    </div>
  );
}
