// ── Query History Tab — NL2SQL Management Page ───────────────────────────────

import { useState, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import {
  Table, Button, Space, Tag, message, Typography, Card, Spin,
  Empty, Tooltip, Drawer, Select, Popconfirm, Input,
} from 'antd';
import {
  HistoryOutlined, DeleteOutlined, PlayCircleOutlined,
  CheckCircleOutlined, CloseCircleOutlined, EyeOutlined,
  CopyOutlined, ReloadOutlined, DownloadOutlined,
} from '@ant-design/icons';
import dayjs from 'dayjs';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import type { Nl2sqlQueryHistoryItem } from '@/types';

const { Text } = Typography;
const PAGE_SIZE = 20;

interface SqlDrawerState {
  open: boolean;
  sql: string | null;
  question: string;
}

export function QueryHistoryTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [page, setPage] = useState(1);
  const [filterDsId, setFilterDsId] = useState<string | undefined>(undefined);
  const [filterExecuted, setFilterExecuted] = useState<string>('all');
  const [sqlDrawer, setSqlDrawer] = useState<SqlDrawerState>({ open: false, sql: null, question: '' });

  const { data: dsResponse } = useQuery({
    queryKey: ['datasources-for-history'],
    queryFn: () => dataSourcesApi.list({ per_page: 200 }),
  });

  const dsNameMap = useMemo(() => {
    const map: Record<string, string> = {};
    dsResponse?.data_sources?.forEach((ds: { id: string; name: string }) => { map[ds.id] = ds.name; });
    return map;
  }, [dsResponse]);

  const historyQuery = useQuery({
    queryKey: ['nl2sql-history', page, filterDsId, filterExecuted],
    queryFn: () => {
      const params: { page: number; per_page: number; data_source_id?: string; executed?: boolean } = {
        page,
        per_page: PAGE_SIZE,
      };
      if (filterDsId) params.data_source_id = filterDsId;
      if (filterExecuted === 'executed') params.executed = true;
      else if (filterExecuted === 'failed') params.executed = false;
      return nl2sqlApi.history(params);
    },
    placeholderData: (prev) => prev,
  });

  const deleteMutation = useMutation({
    mutationFn: (queryId: string) => nl2sqlApi.deleteQuery(queryId),
    onSuccess: () => {
      message.success(t('management.queryHistory.deleteSuccess'));
      qc.invalidateQueries({ queryKey: ['nl2sql-history'] });
    },
    onError: () => {
      message.error(t('common.failed'));
    },
  });

  const copySql = (sql: string) => {
    navigator.clipboard.writeText(sql).then(() => {
      message.success(t('common.copied'));
    });
  };

  const exportCsv = () => {
    const rows = historyQuery.data?.queries ?? [];
    if (rows.length === 0) return;
    const headers = ['question', 'datasource', 'status', 'rows_returned', 'execution_ms', 'created_at', 'generated_sql'];
    const escape = (v: unknown) => {
      const s = String(v ?? '').replace(/"/g, '""');
      return /[",\n]/.test(s) ? `"${s}"` : s;
    };
    const lines = [
      headers.join(','),
      ...rows.map((r: Nl2sqlQueryHistoryItem) => [
        escape(r.question),
        escape(dsNameMap[r.data_source_id ?? ''] ?? r.data_source_id ?? ''),
        escape(r.executed ? 'executed' : 'failed'),
        escape(r.rows_returned ?? ''),
        escape(r.execution_ms ?? ''),
        escape(r.created_at),
        escape(r.generated_sql ?? ''),
      ].join(',')),
    ];
    const blob = new Blob([lines.join('\n')], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `nl2sql_history_${new Date().toISOString().slice(0, 10)}.csv`;
    a.click();
    URL.revokeObjectURL(url);
    message.success(t('management.queryHistory.exportSuccess'));
  };

  const openSqlDrawer = (record: Nl2sqlQueryHistoryItem) => {
    setSqlDrawer({ open: true, sql: record.generated_sql, question: record.question });
  };

  const executedOptions = [
    { value: 'all', label: t('management.queryHistory.all') },
    { value: 'executed', label: t('management.queryHistory.executed') },
    { value: 'failed', label: t('management.queryHistory.failed') },
  ];

  const queries = historyQuery.data?.queries ?? [];
  const total = historyQuery.data?.total ?? 0;

  const dsOptions = [
    { value: '', label: t('management.queryHistory.all') },
    ...(dsResponse?.data_sources ?? []).map((ds: { id: string; name: string }) => ({
      value: ds.id,
      label: ds.name,
    })),
  ];

  const columns = [
    {
      title: t('management.queryHistory.question'),
      dataIndex: 'question',
      key: 'question',
      ellipsis: true,
      width: '30%',
      render: (q: string) => (
        <Tooltip title={q} placement="topLeft">
          <Text style={{ fontSize: 13 }}>{q}</Text>
        </Tooltip>
      ),
    },
    {
      title: t('management.queryHistory.datasource'),
      dataIndex: 'data_source_id',
      key: 'data_source_id',
      width: 140,
      render: (dsId: string | null) =>
        dsId ? (dsNameMap[dsId] ?? <Text type="secondary">—</Text>) : (
          <Text type="secondary">—</Text>
        ),
    },
    {
      title: t('management.queryHistory.status'),
      dataIndex: 'executed',
      key: 'executed',
      width: 110,
      render: (executed: boolean, record: Nl2sqlQueryHistoryItem) =>
        executed ? (
          <Tag icon={<CheckCircleOutlined />} color="success">
            {t('management.queryHistory.executed')}
          </Tag>
        ) : (
          <Tooltip title={record.error_message ?? ''} placement="top">
            <Tag icon={<CloseCircleOutlined />} color="error">
              {t('management.queryHistory.failed')}
            </Tag>
          </Tooltip>
        ),
    },
    {
      title: t('management.queryHistory.rowsReturned'),
      dataIndex: 'rows_returned',
      key: 'rows_returned',
      width: 80,
      render: (v: number) => v > 0 ? v.toLocaleString() : '—',
    },
    {
      title: t('management.queryHistory.executionTime'),
      dataIndex: 'execution_ms',
      key: 'execution_ms',
      width: 110,
      render: (v: number) => v > 0 ? `${(v / 1000).toFixed(2)}s` : '—',
    },
    {
      title: t('management.queryHistory.createdAt'),
      dataIndex: 'created_at',
      key: 'created_at',
      width: 160,
      render: (v: string) => dayjs(v).format('YYYY-MM-DD HH:mm'),
    },
    {
      title: t('management.queryHistory.actions'),
      key: 'actions',
      width: 120,
      render: (_: unknown, record: Nl2sqlQueryHistoryItem) => (
        <Space size={4}>
          <Tooltip title={t('management.queryHistory.viewSql')}>
            <Button
              type="text"
              size="small"
              icon={<EyeOutlined />}
              onClick={() => openSqlDrawer(record)}
            />
          </Tooltip>
          <Tooltip title={t('management.queryHistory.rerun')}>
            <Button
              type="text"
              size="small"
              icon={<PlayCircleOutlined />}
              onClick={() => window.open(`/nl2sql?query_id=${record.id}`, '_blank')}
            />
          </Tooltip>
          <Tooltip title={t('management.queryHistory.delete')}>
            <Popconfirm
              title={t('management.queryHistory.deleteConfirm')}
              onConfirm={() => deleteMutation.mutate(record.id)}
              okText={t('common.confirm')}
              cancelText={t('common.cancel')}
            >
              <Button
                type="text"
                size="small"
                danger
                icon={<DeleteOutlined />}
              />
            </Popconfirm>
          </Tooltip>
        </Space>
      ),
    },
  ];

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
      {/* Header */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
        <Select
          placeholder={t('management.queryHistory.filterByDatasource')}
          value={filterDsId ?? ''}
          onChange={(v) => { setFilterDsId(v || undefined); setPage(1); }}
          options={dsOptions}
          style={{ minWidth: 200 }}
          allowClear
        />
        <Select
          value={filterExecuted}
          onChange={(v) => { setFilterExecuted(v); setPage(1); }}
          options={executedOptions}
          style={{ minWidth: 130 }}
        />
        <Button
          icon={<ReloadOutlined />}
          onClick={() => qc.invalidateQueries({ queryKey: ['nl2sql-history'] })}
        >
          {t('common.refresh')}
        </Button>
        <Button
          icon={<DownloadOutlined />}
          onClick={exportCsv}
          disabled={!historyQuery.data?.queries?.length}
        >
          {t('management.queryHistory.exportCsv')}
        </Button>
      </div>

      {/* Table */}
      <Table
        columns={columns}
        dataSource={queries}
        rowKey="id"
        loading={historyQuery.isFetching && !historyQuery.isLoading}
        locale={{
          emptyText: (
            <Empty
              image={<HistoryOutlined style={{ fontSize: 48, color: '#8c8c8c' }} />}
              description={t('management.queryHistory.noHistory')}
            />
          ),
        }}
        pagination={{
          current: page,
          pageSize: PAGE_SIZE,
          total,
          showSizeChanger: false,
          showTotal: (total: number) => t('common.total', { count: total }),
          onChange: (p) => setPage(p),
        }}
      />

      {/* SQL Drawer */}
      <Drawer
        title={t('management.queryHistory.viewSql')}
        placement="right"
        width={600}
        open={sqlDrawer.open}
        onClose={() => setSqlDrawer((s) => ({ ...s, open: false }))}
        extra={
          sqlDrawer.sql ? (
            <Button
              size="small"
              icon={<CopyOutlined />}
              onClick={() => copySql(sqlDrawer.sql!)}
            >
              {t('common.copy')}
            </Button>
          ) : null
        }
      >
        {sqlDrawer.question && (
          <div style={{ marginBottom: 12 }}>
            <Text type="secondary">{t('management.queryHistory.question')}: </Text>
            <Text strong>{sqlDrawer.question}</Text>
          </div>
        )}
        {sqlDrawer.sql ? (
          <pre style={{
            background: 'var(--bg-elevated, #1e1e1e)',
            color: '#d4d4d4',
            padding: 16,
            borderRadius: 8,
            fontSize: 13,
            fontFamily: 'Menlo, Monaco, monospace',
            overflow: 'auto',
            maxHeight: 'calc(100vh - 200px)',
          }}>
            {sqlDrawer.sql}
          </pre>
        ) : (
          <Text type="secondary">{t('management.queryHistory.noSql')}</Text>
        )}
      </Drawer>
    </div>
  );
}
