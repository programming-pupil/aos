import { useState, useEffect, useRef } from 'react';
import {
  Form, Input, Button, Space, Tag, Typography,
  Divider, message, Tooltip, Empty, Spin, Alert, Modal, Table, Badge, Tabs,
} from 'antd';
import {
  EditOutlined, CloseOutlined, CheckCircleFilled,
  SearchOutlined, TableOutlined, FormOutlined, DatabaseOutlined,
  ThunderboltOutlined, SyncOutlined, InfoCircleOutlined,
  DownOutlined, RightOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { ApiError } from '@/api/errors';
import { useTranslation } from 'react-i18next';
import type { DataSourceInfo, RefreshTaskStatus } from '@/types';

// ─── Lazy-load sentinel ───────────────────────────────────────────────────────

/** Invisible element that triggers `onVisible` when scrolled into view. */
function LazyLoadSentinel({ onVisible }: { onVisible: () => void }) {
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some(e => e.isIntersecting)) onVisible();
    }, { rootMargin: '200px' });
    io.observe(el);
    return () => io.disconnect();
  }, [onVisible]);
  return <div ref={ref} style={{ height: 1 }} />;
}

interface SemanticsDrawerProps {
  dataSource: DataSourceInfo;
  onClose: () => void;
  onRefresh: () => void;
  /** Optional handler called when user clicks "Retry failed" after a
   *  partial refresh. Implemented by calling refreshSemanticsAsync with
   *  an explicit list of failed table names. */
  onRetryFailed?: (tables: string[]) => void;
  isRefreshing: boolean;
  refreshProgress?: number;
  refreshTaskId: string | null;
  /** When true, render the inner panes without the outer Modal wrapper. */
  embedded?: boolean;
}

function SemanticsDrawer({
  dataSource,
  onClose,
  onRefresh,
  onRetryFailed,
  isRefreshing,
  refreshProgress,
  refreshTaskId,
  embedded = false,
}: SemanticsDrawerProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [activeTab, setActiveTab] = useState<'columns' | 'tables' | 'datasource'>('columns');
  const [search, setSearch] = useState('');
  const [expandedTables, setExpandedTables] = useState<Set<string>>(new Set());
  const [colVisibleCount, setColVisibleCount] = useState(20);
  const [tableVisibleCount, setTableVisibleCount] = useState(30);

  // Reset lazy counts when search changes
  useEffect(() => {
    setColVisibleCount(20);
    setTableVisibleCount(30);
  }, [search, activeTab]);

  const toggleExpanded = (tableName: string) => {
    setExpandedTables(prev => {
      const next = new Set(prev);
      if (next.has(tableName)) next.delete(tableName);
      else next.add(tableName);
      return next;
    });
  };

  // Column semantics
  const { data: colSemanticsData, isLoading: colLoading } = useQuery({
    queryKey: queryKeys.nl2sql.semantics(dataSource.id),
    queryFn: () => nl2sqlApi.getSemantics(dataSource.id),
    staleTime: 30_000,
  });

  // Table-level semantics
  const { data: tableSemantics, isLoading: tableLoading } = useQuery({
    queryKey: ['nl2sql', 'table-semantics', dataSource.id],
    queryFn: () => nl2sqlApi.getAllTableSemantics(dataSource.id),
    staleTime: 30_000,
  });

  // Datasource-level semantics
  const { data: dsSemantics, isLoading: dsLoading } = useQuery({
    queryKey: ['nl2sql', 'datasource-semantics', dataSource.id],
    queryFn: () => nl2sqlApi.getDatasourceSemantics(dataSource.id),
    staleTime: 30_000,
  });

  // Poll task status when a refresh task is running
  const { data: taskStatus } = useQuery<RefreshTaskStatus>({
    queryKey: ['nl2sql', 'refresh-task', refreshTaskId],
    queryFn: () => nl2sqlApi.getRefreshTaskStatus(refreshTaskId!),
    enabled: !!refreshTaskId,
    refetchInterval: 2000,
  });

  // Invalidate semantics queries when task completes
  useEffect(() => {
    if (taskStatus?.status === 'completed' || taskStatus?.status === 'failed') {
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.semantics(dataSource.id) });
      qc.invalidateQueries({ queryKey: ['nl2sql', 'table-semantics', dataSource.id] });
      qc.invalidateQueries({ queryKey: ['nl2sql', 'datasource-semantics', dataSource.id] });
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
    }
  }, [taskStatus?.status, dataSource.id, qc]);

  // Use internal progress from polling, falling back to prop
  const effectiveProgress = taskStatus?.progress ?? refreshProgress;
  const effectiveRefreshing = !!refreshTaskId;

  // Editing state
  const [editingCol, setEditingCol] = useState<{ table_name: string; column_name: string; value: string } | null>(null);
  const [editingTable, setEditingTable] = useState<{ table_name: string; value: string } | null>(null);
  const [editingDs, setEditingDs] = useState<string | null>(null);

  // Mutations
  const updateColMutation = useMutation({
    mutationFn: (payload: { table_name: string; column_name: string; user_description: string }) =>
      nl2sqlApi.updateSemantics(dataSource.id, payload),
    onSuccess: () => {
      message.success(t('datasources.descriptionSaved'));
      setEditingCol(null);
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.semantics(dataSource.id) });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const updateTableMutation = useMutation({
    mutationFn: (payload: { table_name: string; user_description: string }) =>
      nl2sqlApi.updateTableSemantics(dataSource.id, payload.table_name, payload.user_description),
    onSuccess: () => {
      message.success(t('datasources.descriptionSaved'));
      setEditingTable(null);
      qc.invalidateQueries({ queryKey: ['nl2sql', 'table-semantics', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const updateDsMutation = useMutation({
    mutationFn: (user_description: string) =>
      nl2sqlApi.updateDatasourceSemantics(dataSource.id, user_description),
    onSuccess: () => {
      message.success(t('datasources.descriptionSaved'));
      setEditingDs(null);
      qc.invalidateQueries({ queryKey: ['nl2sql', 'datasource-semantics', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  // Derived data
  const colColumns = colSemanticsData?.columns ?? [];
  const filteredCols = search.trim()
    ? colColumns.filter(c =>
        c.table_name.toLowerCase().includes(search.toLowerCase()) ||
        c.column_name.toLowerCase().includes(search.toLowerCase()) ||
        c.ai_description.toLowerCase().includes(search.toLowerCase())
      )
    : colColumns;
  const groupedCols = filteredCols.reduce<Record<string, typeof filteredCols>>((acc, col) => {
    if (!acc[col.table_name]) acc[col.table_name] = [];
    acc[col.table_name].push(col);
    return acc;
  }, {});
  const indexedCount = colColumns.filter(c => c.is_indexed).length;
  const totalColumnCount = colColumns.length;
  const totalTableCount = (tableSemantics ?? []).length;
  const indexedTableCount = (tableSemantics ?? []).filter(t => t.is_indexed).length;

  const filteredTables = (tableSemantics ?? []).filter(t =>
    !search.trim() ||
    t.table_name.toLowerCase().includes(search.toLowerCase()) ||
    t.ai_description.toLowerCase().includes(search.toLowerCase())
  );

  const tabItems = [
    {
      key: 'columns',
      label: (
        <span>
          <FormOutlined />
          {` ${t('datasources.tabColumns')}`}
          {totalColumnCount > 0 && (
            <Badge
              count={`${indexedCount}/${totalColumnCount}`}
              size="small"
              style={{
                marginLeft: 6,
                background: indexedCount === totalColumnCount ? '#52c41a' : '#faad14',
              }}
              overflowCount={9999}
            />
          )}
        </span>
      ),
    },
    {
      key: 'tables',
      label: (
        <span>
          <TableOutlined />
          {` ${t('datasources.tabTables')}`}
          {totalTableCount > 0 && (
            <Badge
              count={`${indexedTableCount}/${totalTableCount}`}
              size="small"
              style={{
                marginLeft: 6,
                background: indexedTableCount === totalTableCount ? '#52c41a' : '#faad14',
              }}
              overflowCount={9999}
            />
          )}
        </span>
      ),
    },
    {
      key: 'datasource',
      label: (
        <span>
          <DatabaseOutlined />
          {` ${t('datasources.tabDatasource')}`}
        </span>
      ),
    },
  ];

  const renderProgressBar = () => {
    if (effectiveProgress === undefined) return null;
    const failed = taskStatus?.failed_tables ?? null;
    const hasFailures = Array.isArray(failed) && failed.length > 0;
    const processed = taskStatus?.processed_tables ?? 0;
    return (
      <div style={{ marginBottom: 12 }}>
        <div style={{ marginBottom: 4, fontSize: 12, color: 'var(--text-secondary)' }}>
          {effectiveRefreshing
            ? `${t('datasources.refreshProgress')}: ${effectiveProgress}%`
            : t('datasources.refreshComplete')}
          {(!effectiveRefreshing || processed > 0) && (
            <span style={{ marginLeft: 8, fontSize: 11, color: 'var(--text-muted)' }}>
              {processed} tables · {hasFailures ? `${failed!.length} failed` : '0 failed'}
            </span>
          )}
        </div>
        <div style={{ height: 6, background: 'var(--bg-secondary)', borderRadius: 3, overflow: 'hidden' }}>
          <div style={{
            height: '100%',
            width: `${effectiveProgress}%`,
            background: hasFailures ? '#faad14' : (effectiveProgress === 100 ? '#52c41a' : '#7c3aed'),
            borderRadius: 3,
            transition: 'width 0.3s ease',
          }} />
        </div>
        {hasFailures && (
          <div style={{ marginTop: 8, padding: 8, background: '#fffbe6', border: '1px solid #ffe58f', borderRadius: 4, fontSize: 12 }}>
            <div style={{ marginBottom: 4 }}>
              {t('datasources.partialFailureTip')}
            </div>
            <ul style={{ margin: 0, paddingLeft: 16, maxHeight: 80, overflow: 'auto' }}>
              {failed!.slice(0, 10).map((f) => (
                <li key={f.table} style={{ color: 'var(--text-muted)' }}>
                  <Typography.Text code style={{ fontSize: 11 }}>{f.table}</Typography.Text>: {f.error}
                </li>
              ))}
              {failed!.length > 10 && (
                <li style={{ color: 'var(--text-muted)' }}>… and {failed!.length - 10} more</li>
              )}
            </ul>
            <Button
              size="small"
              style={{ marginTop: 6 }}
              onClick={() => onRetryFailed?.(failed!.map(f => f.table))}
            >
              {t('datasources.retryFailed')}
            </Button>
          </div>
        )}
      </div>
    );
  };

  const renderColumnTab = () => (
    <>
      <Input
        prefix={<SearchOutlined />}
        placeholder={t('datasources.searchColumn')}
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ marginBottom: 12 }}
        allowClear
      />
      {renderProgressBar()}
      {colLoading ? (
        <div style={{ textAlign: 'center', padding: 32 }}><Spin /></div>
      ) : colColumns.length === 0 ? (
        <Empty description={t('datasources.noColumnsIndexed')} />
      ) : (
        <div style={{ maxHeight: 480, overflow: 'auto' }}>
          {Object.entries(groupedCols).slice(0, colVisibleCount).map(([tableName, cols]) => {
            const isExpanded = expandedTables.has(tableName);
            return (
              <div key={tableName} style={{ marginBottom: 16, border: '1px solid var(--border-subtle)', borderRadius: 8, overflow: 'hidden' }}>
                <div
                  style={{ padding: '8px 12px', background: 'var(--bg-elevated)', fontWeight: 600, fontSize: 13, display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', userSelect: 'none' }}
                  onClick={() => toggleExpanded(tableName)}
                >
                  {isExpanded
                    ? <DownOutlined style={{ color: 'var(--text-muted)', fontSize: 11, flexShrink: 0 }} />
                    : <RightOutlined style={{ color: 'var(--text-muted)', fontSize: 11, flexShrink: 0 }} />
                  }
                  <TableOutlined style={{ color: '#7c3aed' }} />
                  {tableName}
                  <Tag style={{ marginLeft: 'auto', fontSize: 10 }}>{cols.length} cols</Tag>
                </div>
                {isExpanded && (
                  <Table
                    size="small"
                    pagination={false}
                    dataSource={cols}
                    rowKey="column_name"
                    columns={[
                      {
                        title: t('datasources.columnName'),
                        dataIndex: 'column_name',
                        key: 'column_name',
                        width: 120,
                        render: (v: string, record: { table_name: string; column_name: string; is_indexed: boolean }) => (
                          <Space>
                            <Typography.Text code style={{ fontSize: 12 }}>{v}</Typography.Text>
                            {editingCol?.column_name !== record.column_name && (
                              <Tag color={record.is_indexed ? 'green' : 'default'} style={{ fontSize: 10 }}>
                                {record.is_indexed ? t('nl2sql.indexed') : '—'}
                              </Tag>
                            )}
                          </Space>
                        ),
                      },
                      {
                        title: (
                          <Tooltip title={t('datasources.descriptionEditHint')}>
                            {t('datasources.description')} <InfoCircleOutlined style={{ fontSize: 11, color: 'var(--text-muted)' }} />
                          </Tooltip>
                        ),
                        dataIndex: 'ai_description',
                        key: 'ai_description',
                        render: (v: string, record: { table_name: string; column_name: string; is_indexed: boolean }) => {
                          const isEditing = editingCol?.column_name === record.column_name;
                          if (isEditing) {
                            return (
                              <Space.Compact style={{ width: '100%' }}>
                                <Input
                                  size="small"
                                  value={editingCol?.value ?? ''}
                                  onChange={(e) => setEditingCol({ ...editingCol, value: e.target.value })}
                                  placeholder={t('datasources.userDescriptionPlaceholder')}
                                  onPressEnter={() => {
                                    if (editingCol) {
                                      updateColMutation.mutate({
                                        table_name: editingCol.table_name,
                                        column_name: editingCol.column_name,
                                        user_description: editingCol.value,
                                      });
                                    }
                                  }}
                                />
                                <Button size="small" type="primary" onClick={() => {
                                  if (editingCol) {
                                    updateColMutation.mutate({
                                      table_name: editingCol.table_name,
                                      column_name: editingCol.column_name,
                                      user_description: editingCol.value,
                                    });
                                  }
                                }}>
                                  <CheckCircleFilled style={{ fontSize: 11 }} />
                                </Button>
                                <Button size="small" onClick={() => setEditingCol(null)}>
                                  <CloseOutlined />
                                </Button>
                              </Space.Compact>
                            );
                          }
                          return (
                            <Tooltip title={v ? t('datasources.clickToEdit') : t('datasources.clickToEdit')}>
                              <Typography.Text
                                style={{ fontSize: 12, color: v ? 'var(--text-primary)' : 'var(--text-muted)', fontStyle: v ? 'normal' : 'italic', cursor: 'pointer' }}
                                onClick={() => setEditingCol({ table_name: record.table_name, column_name: record.column_name, value: v ?? '' })}
                              >
                                {v || t('datasources.addDescription')}
                              </Typography.Text>
                            </Tooltip>
                          );
                        },
                      },
                    ]}
                  />
                )}
              </div>
            );
          })}
          {Object.keys(groupedCols).length > colVisibleCount && (
            <LazyLoadSentinel onVisible={() => setColVisibleCount(c => c + 20)} />
          )}
        </div>
      )}
    </>
  );

  const renderTableTab = () => (
    <>
      <Input
        prefix={<SearchOutlined />}
        placeholder={t('datasources.searchTable')}
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ marginBottom: 12 }}
        allowClear
      />
      {renderProgressBar()}
      {tableLoading ? (
        <div style={{ textAlign: 'center', padding: 32 }}><Spin /></div>
      ) : filteredTables.length === 0 ? (
        <Empty description={t('datasources.noTablesIndexed')} />
      ) : (
        <div style={{ maxHeight: 480, overflow: 'auto' }}>
          {filteredTables.slice(0, tableVisibleCount).map((tbl) => {
            const isEditing = editingTable?.table_name === tbl.table_name;
            return (
              <div key={tbl.table_name} style={{ marginBottom: 12, border: '1px solid var(--border-subtle)', borderRadius: 8, overflow: 'hidden' }}>
                <div style={{ padding: '8px 12px', background: 'var(--bg-elevated)', display: 'flex', alignItems: 'center', gap: 8 }}>
                  <TableOutlined style={{ color: '#7c3aed' }} />
                  <Typography.Text strong style={{ fontSize: 13 }}>{tbl.table_name}</Typography.Text>
                  <Tag color={tbl.is_indexed ? 'green' : 'default'} style={{ fontSize: 10, marginLeft: 'auto' }}>
                    {tbl.is_indexed ? t('nl2sql.indexed') : '—'}
                  </Tag>
                  <Tooltip title={t('datasources.descriptionEditHint')}>
                    <Button
                      size="small"
                      icon={<EditOutlined />}
                      onClick={() => setEditingTable({ table_name: tbl.table_name, value: tbl.ai_description })}
                    >
                      {t('common.edit')}
                    </Button>
                  </Tooltip>
                </div>
                <div style={{ padding: 12 }}>
                  {isEditing ? (
                    <Space direction="vertical" style={{ width: '100%' }}>
                      <Typography.Text type="secondary" style={{ fontSize: 11 }}>{t('datasources.description')}:</Typography.Text>
                      <Input.TextArea
                        value={editingTable?.value ?? ''}
                        onChange={(e) => setEditingTable({ ...editingTable, value: e.target.value })}
                        placeholder={t('datasources.tableDescriptionPlaceholder')}
                        rows={3}
                      />
                      <Space>
                        <Button type="primary" size="small" loading={updateTableMutation.isPending}
                          onClick={() => {
                            if (editingTable) {
                              updateTableMutation.mutate({ table_name: editingTable.table_name, user_description: editingTable.value });
                            }
                          }}>
                          {t('common.save')}
                        </Button>
                        <Button size="small" onClick={() => setEditingTable(null)}>{t('common.cancel')}</Button>
                      </Space>
                    </Space>
                  ) : (
                    <Space direction="vertical" style={{ width: '100%' }}>
                      {tbl.ai_description ? (
                        <Typography.Text style={{ fontSize: 12, color: 'var(--text-primary)', display: 'block' }}>{tbl.ai_description}</Typography.Text>
                      ) : (
                        <Typography.Text style={{ fontSize: 12, color: 'var(--text-muted)', fontStyle: 'italic' }}>
                          {t('datasources.noUserTableDescription')}
                        </Typography.Text>
                      )}
                    </Space>
                  )}
                </div>
              </div>
            );
          })}
          {filteredTables.length > tableVisibleCount && (
            <LazyLoadSentinel onVisible={() => setTableVisibleCount(c => c + 30)} />
          )}
        </div>
      )}
    </>
  );

  const renderDatasourceTab = () => {
    const displayedDescription = dsSemantics?.ai_description || dataSource.description || '';
    const hasDescription = !!displayedDescription;
    return (
      <>
        {renderProgressBar()}
        {dsLoading ? (
          <div style={{ textAlign: 'center', padding: 32 }}><Spin /></div>
        ) : (
          <div style={{ maxHeight: 480, overflow: 'auto' }}>
            <Alert
              type="info"
              message={t('datasources.datasourceDescriptionTip')}
              showIcon
              icon={<InfoCircleOutlined />}
              style={{ marginBottom: 16 }}
            />
            <div>
              <Typography.Text type="secondary" style={{ fontSize: 11, display: 'block', marginBottom: 6 }}>
                {t('datasources.description')}
                <Tooltip title={t('datasources.descriptionEditHint')}>
                  <InfoCircleOutlined style={{ marginLeft: 4, fontSize: 11, color: 'var(--text-muted)' }} />
                </Tooltip>
              </Typography.Text>
              {editingDs !== null ? (
                <Space direction="vertical" style={{ width: '100%' }}>
                  <Input.TextArea
                    value={editingDs}
                    onChange={(e) => setEditingDs(e.target.value)}
                    placeholder={t('datasources.datasourceDescriptionPlaceholder')}
                    rows={4}
                  />
                  <Space>
                    <Button type="primary" size="small" loading={updateDsMutation.isPending}
                      onClick={() => updateDsMutation.mutate(editingDs)}>
                      {t('common.save')}
                    </Button>
                    <Button size="small" onClick={() => setEditingDs(null)}>{t('common.cancel')}</Button>
                  </Space>
                </Space>
              ) : (
                <div>
                  {hasDescription ? (
                    <div>
                      <Typography.Text style={{ fontSize: 13 }}>{displayedDescription}</Typography.Text>
                      <Button size="small" icon={<EditOutlined />} style={{ marginLeft: 8 }}
                        onClick={() => setEditingDs(displayedDescription)}>
                        {t('common.edit')}
                      </Button>
                    </div>
                  ) : (
                    <Space>
                      <Typography.Text style={{ fontSize: 12, color: 'var(--text-muted)', fontStyle: 'italic' }}>
                        {t('datasources.noUserDsDescription')}
                      </Typography.Text>
                      <Button size="small" type="primary" icon={<EditOutlined />}
                        onClick={() => setEditingDs('')}>
                        {t('datasources.addDescription')}
                      </Button>
                    </Space>
                  )}
                </div>
              )}
            </div>
          </div>
        )}
      </>
    );
  };

  return (
    embedded ? (
      <>
        <Tabs
          activeKey={activeTab}
          onChange={(key) => { setActiveTab(key as typeof activeTab); setSearch(''); }}
          items={tabItems}
          style={{ marginTop: -8 }}
        />
        <Divider style={{ margin: '8px 0' }} />
        {activeTab === 'columns' && renderColumnTab()}
        {activeTab === 'tables' && renderTableTab()}
        {activeTab === 'datasource' && renderDatasourceTab()}
      </>
    ) : (
      <Modal
        title={
          <Space>
            <ThunderboltOutlined style={{ color: '#7c3aed' }} />
            {t('datasources.semanticIndex')}
            <Tag color="purple">{dataSource.name}</Tag>
          </Space>
        }
        open
        onCancel={onClose}
        footer={
          <Space>
            <Button onClick={onClose}>{t('common.close')}</Button>
            <Button
              type="primary"
              icon={<SyncOutlined spin={isRefreshing} />}
              loading={isRefreshing}
              onClick={onRefresh}
            >
              {t('datasources.refreshIndex')}
            </Button>
          </Space>
        }
        width={820}
      >
        <Tabs
          activeKey={activeTab}
          onChange={(key) => { setActiveTab(key as typeof activeTab); setSearch(''); }}
          items={tabItems}
          style={{ marginTop: -8 }}
        />
        <Divider style={{ margin: '8px 0' }} />
        {activeTab === 'columns' && renderColumnTab()}
        {activeTab === 'tables' && renderTableTab()}
        {activeTab === 'datasource' && renderDatasourceTab()}
      </Modal>
    )
  );
}

export { SemanticsDrawer };
