// ── NL2SQL Schema Changes — pending schema change notifications ───────────────────
// Lists, reviews, and approves/rejects schema change notifications.
// When approved, triggers re-indexing of the affected data source.

import { useCallback, useState } from 'react';
import {
  Layout, Table, Tag, Button, Space, Modal, Typography, Card,
  message, Empty, Descriptions, Badge, Tabs, Drawer,
  Tooltip, Divider, Alert,
} from 'antd';
import {
  CheckOutlined, CloseOutlined, EyeOutlined, WarningOutlined,
  SafetyOutlined, InfoCircleOutlined, DeleteOutlined, ReloadOutlined,
  ClockCircleOutlined, ExclamationCircleOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { SchemaChangeNotification, SchemaChangeDetailResponse } from '@/types';
import dayjs from 'dayjs';
import relativeTime from 'dayjs/plugin/relativeTime';
import { useTranslation } from 'react-i18next';
import { ErrorBoundary } from '@/components/ErrorBoundary';
import { useTabRefresh } from '@/hooks/useTabRefresh';
import { usePermissions } from '@/store/permissions';

dayjs.extend(relativeTime);

const { Text, Title, Paragraph } = Typography;

function useSchemaChanges(status: string) {
  return useQuery({
    queryKey: queryKeys.nl2sql.schemaChanges.list({ status, page: 1, per_page: 50 }),
    queryFn: () => nl2sqlApi.listSchemaChanges({ status, page: 1, per_page: 50 }),
    refetchInterval: 30_000,
  });
}

// ─── Helpers ──────────────────────────────────────────────────────────────

function ChangeTypeTag({ type }: { type: string }) {
  const { t } = useTranslation();
  const map: Record<string, { color: string; labelKey: string }> = {
    tables_added: { color: 'green', labelKey: 'schemaChanges.changeType.tablesAdded' },
    tables_removed: { color: 'red', labelKey: 'schemaChanges.changeType.tablesRemoved' },
    tables_changed: { color: 'orange', labelKey: 'schemaChanges.changeType.tablesChanged' },
    columns_added: { color: 'cyan', labelKey: 'schemaChanges.changeType.columnsAdded' },
    columns_removed: { color: 'volcano', labelKey: 'schemaChanges.changeType.columnsRemoved' },
    columns_changed: { color: 'gold', labelKey: 'schemaChanges.changeType.columnsChanged' },
    types_changed: { color: 'purple', labelKey: 'schemaChanges.changeType.typesChanged' },
  };
  const m = map[type] ?? { color: 'default', labelKey: 'schemaChanges.changeType.unknown' };
  return <Tag color={m.color}>{t(m.labelKey)}</Tag>;
}

function StatusTag({ status }: { status: string }) {
  const { t } = useTranslation();
  const map: Record<string, { color: string; icon: React.ReactNode; labelKey: string }> = {
    pending: { color: 'warning', icon: <ClockCircleOutlined />, labelKey: 'schemaChanges.statusTag.pending' },
    approved: { color: 'success', icon: <CheckOutlined />, labelKey: 'schemaChanges.statusTag.approved' },
    rejected: { color: 'error', icon: <CloseOutlined />, labelKey: 'schemaChanges.statusTag.rejected' },
    completed: { color: 'blue', icon: <SafetyOutlined />, labelKey: 'schemaChanges.statusTag.completed' },
  };
  const m = map[status] ?? { color: 'default', icon: null, labelKey: 'schemaChanges.statusTag.unknown' };
  return (
    <Tag color={m.color} icon={m.icon}>
      {t(m.labelKey)}
    </Tag>
  );
}

function ActionTag({ action }: { action: string }) {
  const { t } = useTranslation();
  const map: Record<string, { color: string; labelKey: string }> = {
    reindex: { color: 'blue', labelKey: 'schemaChanges.action.reindex' },
    review_semantics: { color: 'orange', labelKey: 'schemaChanges.action.reviewSemantics' },
    no_action: { color: 'default', labelKey: 'schemaChanges.action.noAction' },
  };
  const m = map[action] ?? { color: 'default', labelKey: 'schemaChanges.action.unknown' };
  return <Tag color={m.color}>{t(m.labelKey)}</Tag>;
}

function DatasourceName({ dsId }: { dsId: string }) {
  const { data: dsList } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list({ per_page: 200 }),
    staleTime: 5 * 60 * 1000,
  });
  const ds = dsList?.data_sources?.find((d) => d.id === dsId);
  return <Text>{ds?.name ?? dsId}</Text>;
}

// ─── Change Detail Drawer ────────────────────────────────────────────────────────

function ChangeDetailDrawer({
  notification,
  open,
  onClose,
  onApprove,
  onReject,
}: {
  notification: SchemaChangeDetailResponse | null;
  open: boolean;
  onClose: () => void;
  onApprove: (id: number) => void;
  onReject: (id: number) => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  // Schema-change approve/reject are admin-only operations on the backend
  // (rust/.../schema_changes.rs::approve_schema_change has require_admin(&claims)?).
  // Mirror that constraint here so non-admins do not see disabled-but-tantalizing buttons.
  const canWrite = usePermissions((s) => s.hasPermission('nl2sql:write'));

  const approve = useMutation({
    mutationFn: (id: number) => nl2sqlApi.approveSchemaChange(id),
    onSuccess: () => {
      message.success(t('schemaChanges.approveSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.schemaChanges.all() });
      onClose();
    },
    onError: (e: Error) => message.error(e?.message ?? t('schemaChanges.approveFailed')),
  });

  const reject = useMutation({
    mutationFn: (id: number) => nl2sqlApi.rejectSchemaChange(id),
    onSuccess: () => {
      message.success(t('schemaChanges.rejectSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.schemaChanges.all() });
      onClose();
    },
    onError: (e: Error) => message.error(e?.message ?? t('schemaChanges.rejectFailed')),
  });

  if (!notification) return null;

  return (
    <Drawer
      title={
        <Space>
          <SafetyOutlined />
          <span>{t('schemaChanges.changeDetail')} — <DatasourceName dsId={notification.datasourceId} /></span>
          <ChangeTypeTag type={notification.changeType} />
          <StatusTag status={notification.status} />
        </Space>
      }
      placement="right"
      width={600}
      onClose={onClose}
      open={open}
      extra={
        notification.status === 'pending' && canWrite && (
          <Space>
            <Button
              danger
              icon={<CloseOutlined />}
              onClick={() => onReject(notification.id)}
              loading={reject.isPending}
            >
              {t('common.reject')}
            </Button>
            <Button
              type="primary"
              icon={<CheckOutlined />}
              onClick={() => onApprove(notification.id)}
              loading={approve.isPending}
            >
              {t('schemaChanges.approveAndReindex')}
            </Button>
          </Space>
        )
      }
    >
      <Descriptions column={2} bordered size="small">
        <Descriptions.Item label={t('schemaChanges.changeTypeLabel')}>
          <ChangeTypeTag type={notification.changeType} />
        </Descriptions.Item>
        <Descriptions.Item label={t('schemaChanges.recommendedAction')}>
          <ActionTag action={notification.recommendedAction} />
        </Descriptions.Item>
        <Descriptions.Item label={t('schemaChanges.affectedQueries')}>
          <Badge count={notification.affectedQueriesCount} showZero style={{ backgroundColor: notification.affectedQueriesCount > 10 ? '#ff4d4f' : '#1890ff' }} />
        </Descriptions.Item>
        <Descriptions.Item label={t('schemaChanges.createdAt')}>
          {dayjs(notification.createdAt).format('YYYY-MM-DD HH:mm:ss')}
        </Descriptions.Item>
        <Descriptions.Item label={t('schemaChanges.status')}>
          <StatusTag status={notification.status} />
        </Descriptions.Item>
        <Descriptions.Item label={t('schemaChanges.reviewer')}>
          {notification.reviewedBy || <Text type="secondary">{t('schemaChanges.pendingReview')}</Text>}
        </Descriptions.Item>
      </Descriptions>

      <Divider orientation="left">{t('schemaChanges.changeDetails')}</Divider>

      <div style={{ marginBottom: 16 }}>
        {notification.details.length === 0 && (
          <Empty description={t('schemaChanges.noDetails')} image={Empty.PRESENTED_IMAGE_SIMPLE} />
        )}
        {notification.details.map((item, i) => (
          <Card size="small" key={i} style={{ marginBottom: 8 }}>
            <Space>
              <Tag>{item.table ?? t('common.na')}</Tag>
              {item.column && <Tag color="blue">{item.column}</Tag>}
              {item.oldValue && (
                <>
                  <Text type="secondary">{t('schemaChanges.oldValue')}：</Text>
                  <Text code>{item.oldValue}</Text>
                </>
              )}
              {item.newValue && (
                <>
                  <Text type="secondary">{t('schemaChanges.newValue')}：</Text>
                  <Text code>{item.newValue}</Text>
                </>
              )}
            </Space>
          </Card>
        ))}
      </div>

      {notification.affectedQueriesCount > 0 && (
        <>
          <Divider orientation="left">
            <Space>
              <WarningOutlined />
              <span>{t('schemaChanges.affectedHistoryQueries', { count: notification.affectedQueriesCount })}</span>
            </Space>
          </Divider>
          {notification.affectedQueries.map((q) => (
            <Card size="small" key={q.queryId} style={{ marginBottom: 8 }}>
              <Text type="secondary">Q: </Text>
              <Text>{q.question ?? t('common.na')}</Text>
              {q.generatedSql && (
                <>
                  <br />
                  <Text type="secondary">SQL: </Text>
                  <Text code style={{ fontSize: 11 }}>{q.generatedSql.slice(0, 120)}{q.generatedSql.length > 120 ? '...' : ''}</Text>
                </>
              )}
            </Card>
          ))}
        </>
      )}

      {notification.recommendedAction === 'reindex' && (
        <Alert
          message={t('schemaChanges.alertReindex.message')}
          description={t('schemaChanges.alertReindex.description')}
          type="info"
          showIcon
          style={{ marginTop: 16 }}
        />
      )}

      {notification.recommendedAction === 'review_semantics' && (
        <Alert
          message={t('schemaChanges.alertReviewSemantics.message')}
          description={t('schemaChanges.alertReviewSemantics.description')}
          type="warning"
          showIcon
          style={{ marginTop: 16 }}
        />
      )}
    </Drawer>
  );
}

// ─── Changes Table ────────────────────────────────────────────────────────────

function ChangesTable({ status }: { status: string }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const canWrite = usePermissions((s) => s.hasPermission('nl2sql:write'));
  const [detailId, setDetailId] = useState<number | null>(null);
  const [detail, setDetail] = useState<SchemaChangeDetailResponse | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);

  const { data, isLoading } = useSchemaChanges(status);

  const approve = useMutation({
    mutationFn: (id: number) => nl2sqlApi.approveSchemaChange(id),
    onSuccess: () => {
      message.success(t('schemaChanges.approveSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.schemaChanges.all() });
    },
    onError: (e: Error) => message.error(e?.message ?? t('schemaChanges.approveFailed')),
  });

  const reject = useMutation({
    mutationFn: (id: number) => nl2sqlApi.rejectSchemaChange(id),
    onSuccess: () => {
      message.success(t('schemaChanges.rejectSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.schemaChanges.all() });
    },
    onError: (e: Error) => message.error(e?.message ?? t('schemaChanges.rejectFailed')),
  });

  const openDetail = async (id: number) => {
    try {
      const d = await nl2sqlApi.getSchemaChangeDetail(id);
      setDetail(d);
      setDetailId(id);
      setDrawerOpen(true);
    } catch (e) {
      message.error(t('schemaChanges.fetchDetailFailed'));
    }
  };

  const handleApprove = (id: number) => approve.mutate(id);
  const handleReject = (id: number) => reject.mutate(id);

  const columns = [
    {
      title: t('schemaChanges.datasource'),
      dataIndex: 'datasourceId',
      key: 'datasourceId',
      width: 160,
      render: (dsId: string) => <DatasourceName dsId={dsId} />,
    },
    {
      title: t('schemaChanges.changeTypeLabel'),
      dataIndex: 'changeType',
      key: 'changeType',
      width: 120,
      render: (t: string) => <ChangeTypeTag type={t} />,
    },
    {
      title: t('schemaChanges.recommendedAction'),
      dataIndex: 'recommendedAction',
      key: 'recommendedAction',
      width: 130,
      render: (a: string) => <ActionTag action={a} />,
    },
    {
      title: t('schemaChanges.affectedQueries'),
      dataIndex: 'affectedQueriesCount',
      key: 'affectedQueriesCount',
      width: 110,
      render: (n: number) => (
        <Badge count={n} showZero style={{ backgroundColor: n > 10 ? '#ff4d4f' : '#1890ff' }} />
      ),
    },
    {
      title: t('schemaChanges.status'),
      dataIndex: 'status',
      key: 'status',
      width: 90,
      render: (s: string) => <StatusTag status={s} />,
    },
    {
      title: t('schemaChanges.createdAt'),
      dataIndex: 'createdAt',
      key: 'createdAt',
      width: 140,
      render: (ts: string) => (
        <Tooltip title={dayjs(ts).format('YYYY-MM-DD HH:mm:ss')}>
          <Text type="secondary">{dayjs(ts).fromNow()}</Text>
        </Tooltip>
      ),
    },
    {
      title: t('schemaChanges.reviewer'),
      dataIndex: 'reviewedBy',
      key: 'reviewedBy',
      width: 100,
      render: (u: string) => u || <Text type="secondary">—</Text>,
    },
    {
      title: t('schemaChanges.actions'),
      key: 'actions',
      width: 200,
      render: (_: unknown, record: SchemaChangeNotification) => (
        <Space size="small">
          <Button
            size="small"
            icon={<EyeOutlined />}
            onClick={() => openDetail(record.id)}
          >
            {t('schemaChanges.viewDetail')}
          </Button>
          {record.status === 'pending' && canWrite && (
            <>
              <Button
                size="small"
                type="primary"
                icon={<CheckOutlined />}
                onClick={() => handleApprove(record.id)}
                loading={approve.isPending}
              >
                {t('schemaChanges.approve')}
              </Button>
              <Button
                size="small"
                danger
                icon={<CloseOutlined />}
                onClick={() => handleReject(record.id)}
                loading={reject.isPending}
              >
                {t('common.reject')}
              </Button>
            </>
          )}
        </Space>
      ),
    },
  ];

  const emptyText = status === 'pending'
    ? t('schemaChanges.emptyPending')
    : t(status === 'approved' ? 'schemaChanges.emptyApproved' : 'schemaChanges.emptyRejected');

  return (
    <div>
      <Table
        dataSource={data?.changes ?? []}
        columns={columns}
        rowKey="id"
        loading={isLoading}
        pagination={{ pageSize: 20 }}
        size="small"
        locale={{
          emptyText: (
            <Empty
              description={emptyText}
              image={Empty.PRESENTED_IMAGE_SIMPLE}
            />
          ),
        }}
      />

      <ChangeDetailDrawer
        notification={detail}
        open={drawerOpen}
        onClose={() => { setDrawerOpen(false); setDetail(null); }}
        onApprove={handleApprove}
        onReject={handleReject}
      />
    </div>
  );
}

// ─── Main Page ────────────────────────────────────────────────────────────────

export default function Nl2sqlSchemaChanges() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [activeTab, setActiveTab] = useState('pending');
  const onActiveTabRefresh = useCallback((key: string) => {
    void key;
    qc.invalidateQueries({ queryKey: queryKeys.nl2sql.schemaChanges.all() });
  }, [qc]);
  const handleTabClick = useTabRefresh(activeTab, onActiveTabRefresh);

  return (
    <ErrorBoundary>
    <Layout style={{ minHeight: '100vh', background: 'var(--bg-void)' }}>
      <Layout.Content style={{ padding: '24px 24px', maxWidth: 1200, margin: '0 auto', width: '100%' }}>
        <div style={{ marginBottom: 24 }}>
          <Title level={4} style={{ margin: 0, color: 'var(--text-primary)' }}>
            <SafetyOutlined style={{ marginRight: 8 }} />
            {t('schemaChanges.pageTitle')}
          </Title>
          <Text type="secondary">
            {t('schemaChanges.pageDesc')}
          </Text>
        </div>

        <Card style={{ background: 'var(--bg-surface)' }} bodyStyle={{ padding: 0 }}>
          <Tabs
            activeKey={activeTab}
            onChange={setActiveTab}
            onTabClick={handleTabClick}
            tabBarStyle={{ padding: '0 24px' }}
            items={[
              {
                key: 'pending',
                label: (
                  <span>
                    <Badge status="warning" />
                    {t('schemaChanges.statusTag.pending')}
                  </span>
                ),
                children: <div style={{ padding: '16px 24px' }}><ChangesTable status="pending" /></div>,
              },
              {
                key: 'approved',
                label: (
                  <span>
                    <Badge status="success" />
                    {t('schemaChanges.statusTag.approved')}
                  </span>
                ),
                children: <div style={{ padding: '16px 24px' }}><ChangesTable status="approved" /></div>,
              },
              {
                key: 'rejected',
                label: (
                  <span>
                    <Badge status="error" />
                    {t('schemaChanges.statusTag.rejected')}
                  </span>
                ),
                children: <div style={{ padding: '16px 24px' }}><ChangesTable status="rejected" /></div>,
              },
            ]}
          />
        </Card>
      </Layout.Content>
    </Layout>
    </ErrorBoundary>
  );
}
