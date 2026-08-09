import { useState, useEffect, useCallback, useRef } from 'react';
import { useNavigate } from '@/router';
import {
  Card, Table, Typography, Button, Modal, Form, Input, Select, Tag, Space, message,
  Empty, Popconfirm, Tabs, Spin, Divider, Alert, Tooltip, Drawer, Badge, Switch, InputNumber,
  Checkbox, List, Progress,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined, PlayCircleOutlined,
  DatabaseOutlined, ThunderboltOutlined, CheckCircleFilled,
  SearchOutlined, TableOutlined, SyncOutlined, CloseOutlined,
  SettingOutlined, FormOutlined, InfoCircleOutlined, HolderOutlined,
  RightOutlined, DownOutlined, UploadOutlined,
  ClockCircleOutlined,
} from '@ant-design/icons';
import { DiscoverProgressModal } from '@/components/datasources/DiscoverProgressModal';
import type { ColumnsType } from 'antd/es/table';
import { useTranslation } from 'react-i18next';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { dataSourcesApi, nl2sqlApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { ApiError } from '@/api/errors';
import { PageSkeleton } from '@/components/Skeleton';
import type {
  DataSourceInfo,
  DataSourceDbType,
  DataSourceSchemaInfo,
  TableSemantics,
  DatasourceSemantics,
  ManualColumn,
  ManualTable,
} from '@/types';
import { ErrorBoundary } from '@/components/ErrorBoundary';
import { useTabRefresh } from '@/hooks/useTabRefresh';
import { useDismissibleNotice } from '@/hooks/useDismissibleNotice';

const { Title, Text } = Typography;
const { TextArea } = Input;

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

// ─── Shared DB type configs (defined at module level to avoid recreation) ────

export const DB_TYPES: Record<string, { label: string; types: string[] }> = {
  mysql: {
    label: 'MySQL / TiDB',
    types: [
      'INT', 'BIGINT', 'SMALLINT', 'TINYINT', 'MEDIUMINT',
      'DECIMAL', 'FLOAT', 'DOUBLE',
      'CHAR', 'VARCHAR(255)', 'TEXT', 'MEDIUMTEXT', 'LONGTEXT',
      'DATETIME', 'DATE', 'TIME', 'TIMESTAMP', 'YEAR',
      'BOOLEAN', 'JSON', 'BLOB', 'MEDIUMBLOB', 'LONGBLOB',
      'ENUM', 'SET', 'BIT', 'GEOMETRY',
    ],
  },
  postgres: {
    label: 'PostgreSQL',
    types: [
      'INT', 'BIGINT', 'SMALLINT', 'SERIAL', 'BIGSERIAL', 'OID',
      'DECIMAL', 'NUMERIC', 'REAL', 'DOUBLE PRECISION', 'MONEY',
      'CHAR', 'VARCHAR(255)', 'TEXT',
      'DATE', 'TIME', 'TIMESTAMP', 'TIMESTAMPTZ', 'INTERVAL',
      'BOOLEAN',
      'JSON', 'JSONB', 'UUID', 'XML', 'ARRAY', 'INET', 'CIDR',
    ],
  },
  clickhouse: {
    label: 'ClickHouse',
    types: [
      'Int8', 'Int16', 'Int32', 'Int64', 'Int128', 'UInt8', 'UInt16', 'UInt32', 'UInt64', 'UInt128', 'UInt256',
      'Float32', 'Float64', 'Decimal32', 'Decimal64', 'Decimal128',
      'String', 'FixedString(32)', 'UUID',
      'Date', 'Date32', 'DateTime', 'DateTime64',
      'Enum8', 'Enum16',
      'Array(String)', 'Map(String, String)',
      'Bool', 'Nothing', 'Nullable(Int64)',
    ],
  },
  mongodb: {
    label: 'MongoDB',
    types: [
      'STRING', 'INT32', 'INT64', 'DOUBLE', 'DECIMAL128', 'BOOLEAN',
      'OBJECT_ID', 'DATETIME', 'TIMESTAMP', 'DOCUMENT', 'ARRAY',
      'BINARY', 'REGEX', 'JAVASCRIPT', 'NULL',
    ],
  },
  trino: {
    label: 'Trino / Presto',
    types: [
      'INTEGER', 'BIGINT', 'SMALLINT', 'TINYINT',
      'REAL', 'DOUBLE', 'DECIMAL',
      'VARCHAR', 'CHAR', 'VARBINARY', 'JSON', 'UUID',
      'DATE', 'TIME', 'TIMESTAMP', 'TIMESTAMPTZ', 'INTERVAL',
      'BOOLEAN',
      'ARRAY', 'MAP', 'ROW',
    ],
  },
  hive: {
    label: 'Apache Hive',
    types: [
      'TINYINT', 'SMALLINT', 'INT', 'BIGINT', 'FLOAT', 'DOUBLE', 'DECIMAL',
      'STRING', 'VARCHAR', 'CHAR', 'BOOLEAN',
      'DATE', 'TIMESTAMP', 'BINARY',
      'ARRAY', 'MAP', 'STRUCT', 'UNIONTYPE',
    ],
  },
};

export function ColTypeSelect({ placeholder, typeOptions }: { placeholder?: string; typeOptions: string[] }) {
  const { t } = useTranslation();
  return (
    <Select
      showSearch
      placeholder={placeholder ?? 'Select type'}
      style={{ width: 160 }}
      options={[
        ...typeOptions.map(typeOpt => ({ label: typeOpt, value: typeOpt })),
        { label: `— ${t('datasources.customTypeHint')}`, value: '__custom__', disabled: true },
      ]}
    />
  );
}

// ─── Semantic Index Drawer (tabbed: columns / tables / datasource) ─────────────────────

interface SemanticsDrawerProps {
  dataSource: DataSourceInfo;
  onClose: () => void;
  /** Optional handler called when user clicks "Retry failed" after a
   *  partial refresh. Implemented by calling refreshSemanticsAsync with
   *  an explicit list of failed table names. */
  onRetryFailed?: (tables: string[]) => void;
  /** When true, render the inner panes without the outer Modal wrapper. */
  embedded?: boolean;
}

function SemanticsDrawer({
  dataSource,
  onClose,
  onRetryFailed,
  embedded = false,
}: SemanticsDrawerProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [activeTab, setActiveTab] = useState<'columns' | 'tables' | 'datasource'>('columns');
  const [search, setSearch] = useState('');
  const [expandedTables, setExpandedTables] = useState<Set<string>>(new Set());
  const [colVisibleCount, setColVisibleCount] = useState(20);
  const [tableVisibleCount, setTableVisibleCount] = useState(30);
  const onActiveTabRefresh = useCallback((key: string) => {
    void key;
    qc.invalidateQueries({ queryKey: queryKeys.nl2sql.semantics(dataSource.id) });
    qc.invalidateQueries({ queryKey: ['nl2sql', 'table-semantics', dataSource.id] });
    qc.invalidateQueries({ queryKey: ['nl2sql', 'ds-semantics', dataSource.id] });
  }, [dataSource.id, qc]);
  const handleTabClick = useTabRefresh(activeTab, onActiveTabRefresh);

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
            // Dual counter: indexed / total so users can see at a glance
            // how much of the schema has actually been embedded.
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
                        <Text code style={{ fontSize: 12 }}>{v}</Text>
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
                    render: (v: string, record) => {
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
                            <Button size="small" type="primary" loading={updateColMutation.isPending} onClick={() => {
                              if (editingCol) {
                                updateColMutation.mutate({
                                  table_name: editingCol.table_name,
                                  column_name: editingCol.column_name,
                                  user_description: editingCol.value,
                                });
                              }
                            }}>
                              {!updateColMutation.isPending && <CheckCircleFilled style={{ fontSize: 11 }} />}
                            </Button>
                            <Button size="small" onClick={() => setEditingCol(null)}>
                              <CloseOutlined />
                            </Button>
                          </Space.Compact>
                        );
                      }
                      return (
                        <Tooltip title={t('datasources.clickToEdit')}>
                          <Text
                            style={{ fontSize: 12, color: v ? 'var(--text-primary)' : 'var(--text-muted)', fontStyle: v ? 'normal' : 'italic', cursor: 'pointer' }}
                            onClick={() => setEditingCol({ table_name: record.table_name, column_name: record.column_name, value: v ?? '' })}
                          >
                            {v || t('datasources.addDescription')}
                          </Text>
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
                  <Text strong style={{ fontSize: 13 }}>{tbl.table_name}</Text>
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
                      <Text type="secondary" style={{ fontSize: 11 }}>{t('datasources.description')}:</Text>
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
                        <Text style={{ fontSize: 12, color: 'var(--text-primary)', display: 'block' }}>{tbl.ai_description}</Text>
                      ) : (
                        <Text style={{ fontSize: 12, color: 'var(--text-muted)', fontStyle: 'italic' }}>
                          {t('datasources.noUserTableDescription')}
                        </Text>
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
    const displayedDescription = dsSemantics?.user_description || dsSemantics?.ai_description || dataSource.description || '';
    const hasDescription = !!displayedDescription;
    return (
    <>
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
            <Text type="secondary" style={{ fontSize: 11, display: 'block', marginBottom: 6 }}>
              {t('datasources.description')}
              <Tooltip title={t('datasources.descriptionEditHint')}>
                <InfoCircleOutlined style={{ marginLeft: 4, fontSize: 11, color: 'var(--text-muted)' }} />
              </Tooltip>
            </Text>
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
                    <Text style={{ fontSize: 13 }}>{displayedDescription}</Text>
                    <Button size="small" icon={<EditOutlined />} style={{ marginLeft: 8 }}
                      onClick={() => setEditingDs(displayedDescription)}>
                      {t('common.edit')}
                    </Button>
                  </div>
                ) : (
                  <Space>
                    <Text style={{ fontSize: 12, color: 'var(--text-muted)', fontStyle: 'italic' }}>
                      {t('datasources.noUserDsDescription')}
                    </Text>
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
          onTabClick={handleTabClick}
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
      footer={<Button onClick={onClose}>{t('common.close')}</Button>}
      width={820}
    >
      <Tabs
        activeKey={activeTab}
        onChange={(key) => { setActiveTab(key as typeof activeTab); setSearch(''); }}
        onTabClick={handleTabClick}
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

// ─── Schema Management Drawer (manual table/column CRUD) ──────────────────────────────────

interface SchemaManagementDrawerProps {
  dataSource: DataSourceInfo;
  onClose: () => void;
  /** Kick off a semantic refresh for a single table. */
  onRefreshTable?: (tableName: string) => void;
  isRefetching?: boolean;
  /** When true, render the inner panes without the outer Drawer wrapper. */
  embedded?: boolean;
}

function SchemaManagementDrawer({ dataSource, onClose, onRefreshTable, isRefetching = false, embedded = false }: SchemaManagementDrawerProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [search, setSearch] = useState('');
  const [expandedTables, setExpandedTables] = useState<Set<string>>(new Set());

  // Load current schema
  const { data: dsDetail, isLoading } = useQuery({
    queryKey: ['dataSources', 'detail', dataSource.id],
    queryFn: () => dataSourcesApi.get(dataSource.id),
    staleTime: 30_000,
  });

  type TableSchema = {
    table_name: string;
    description?: string;
    is_manual?: boolean;
    columns: { name: string; type: string; description?: string; nullable?: boolean; is_manual?: boolean; primary_key?: boolean }[];
  };

  const rawSchema = dsDetail?.schema_info;
  const schemas: TableSchema[] = Array.isArray(rawSchema)
    ? rawSchema as TableSchema[]
    : (rawSchema && typeof rawSchema === 'object' && 'tables' in rawSchema && Array.isArray((rawSchema as Record<string, unknown>).tables))
      ? (rawSchema as { tables: TableSchema[] }).tables
      : [];
  const filtered = search.trim()
    ? schemas.filter(s => s.table_name.toLowerCase().includes(search.toLowerCase()))
    : schemas;

  const toggleTable = (tableName: string) => {
    setExpandedTables(prev => {
      const next = new Set(prev);
      if (next.has(tableName)) {
        next.delete(tableName);
      } else {
        next.add(tableName);
      }
      return next;
    });
  };

  // Add table form
  const [addTableOpen, setAddTableOpen] = useState(false);
  const [addTableForm] = Form.useForm();
  const [importSqlOpen, setImportSqlOpen] = useState(false);
  const [importSqlForm] = Form.useForm();

  // Add column form
  const [addColOpen, setAddColOpen] = useState(false);
  const [addColForm] = Form.useForm();
  const [colTargetTable, setColTargetTable] = useState('');
  const [colType, setColType] = useState('');

  // Edit table form
  const [editTableOpen, setEditTableOpen] = useState(false);
  const [editTableForm] = Form.useForm();
  const [editTargetTable, setEditTargetTable] = useState('');

  // Edit column form
  const [editColOpen, setEditColOpen] = useState(false);
  const [editColForm] = Form.useForm();
  const [editColTarget, setEditColTarget] = useState({ table: '', column: '' });
  const [editColType, setEditColType] = useState('');

  // Mutations
  const addTableMutation = useMutation({
    mutationFn: (values: { table_name: string; description?: string; columns: ManualColumn[] }) =>
      dataSourcesApi.addManualTable(dataSource.id, {
        table_name: values.table_name,
        description: values.description,
        columns: values.columns,
      }),
    onSuccess: () => {
      message.success(t('datasources.tableAdded'));
      setAddTableOpen(false);
      addTableForm.resetFields();
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const importSqlMutation = useMutation({
    mutationFn: (values: { sql: string; overwriteExisting?: boolean }) =>
      dataSourcesApi.importSqlSchema(dataSource.id, {
        sql: values.sql,
        overwriteExisting: values.overwriteExisting !== false,
      }),
    onSuccess: (result) => {
      message.success(t('datasources.sqlSchemaImportSuccess', {
        imported: result.imported,
        updated: result.updated,
        skipped: result.skipped,
      }));
      if (result.refreshTaskId) {
        message.info(t('datasources.sqlSchemaImportRefreshStarted'));
      }
      setImportSqlOpen(false);
      importSqlForm.resetFields();
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
    },
  });

  const putTableMutation = useMutation({
    mutationFn: ({ table, data }: { table: string; data: { table_name?: string; description?: string } }) =>
      dataSourcesApi.putManualTable(dataSource.id, table, data),
    onSuccess: () => {
      message.success(t('datasources.tableUpdated'));
      setEditTableOpen(false);
      editTableForm.resetFields();
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const deleteTableMutation = useMutation({
    mutationFn: (table: string) => dataSourcesApi.deleteManualTable(dataSource.id, table),
    onSuccess: () => {
      message.success(t('datasources.tableDeleted'));
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const addColMutation = useMutation({
    mutationFn: (values: { name: string; type: string; description?: string; nullable?: boolean }) =>
      dataSourcesApi.addManualColumn(dataSource.id, colTargetTable, values),
    onSuccess: () => {
      message.success(t('datasources.columnAdded'));
      setAddColOpen(false);
      addColForm.resetFields();
      setColType('');
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const putColMutation = useMutation({
    mutationFn: (values: { name?: string; type?: string; description?: string; nullable?: boolean }) =>
      dataSourcesApi.putManualColumn(dataSource.id, editColTarget.table, editColTarget.column, values),
    onSuccess: () => {
      message.success(t('datasources.columnUpdated'));
      setEditColOpen(false);
      editColForm.resetFields();
      setEditColType('');
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const deleteColMutation = useMutation({
    mutationFn: ({ table, column }: { table: string; column: string }) =>
      dataSourcesApi.deleteManualColumn(dataSource.id, table, column),
    onSuccess: () => {
      message.success(t('datasources.columnDeleted'));
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
  });

  const openAddColumn = (tableName: string) => {
    setColTargetTable(tableName);
    setColType('');
    setAddColOpen(true);
  };

  const openEditTable = (tableName: string, description?: string) => {
    setEditTargetTable(tableName);
    editTableForm.setFieldsValue({ table_name: tableName, description });
    setEditTableOpen(true);
  };

  const openEditColumn = (tableName: string, col: { name: string; type: string; description?: string; nullable?: boolean }) => {
    setEditColTarget({ table: tableName, column: col.name });
    editColForm.setFieldsValue({ name: col.name, description: col.description, nullable: col.nullable ?? true });
    setEditColType(col.type);
    setEditColOpen(true);
  };

  const dbType = (dataSource as any).db_type ?? 'mysql';
  const colTypeConfig = DB_TYPES[dbType] ?? DB_TYPES.mysql;

  const body = (
    <>
      {isLoading ? (
        <div style={{ textAlign: 'center', padding: 40 }}><Spin /></div>
      ) : (
        <>
          <Alert
            type="info"
            message={t('datasources.manualSchemaTip')}
            description={t('datasources.tableCountSummary', { count: filtered.length, cols: filtered.reduce((n, t) => n + (t.columns?.length ?? 0), 0) })}
            showIcon
            style={{ marginBottom: 8 }}
            action={
              <Space>
                <Button size="small" icon={<UploadOutlined />} onClick={() => setImportSqlOpen(true)}>
                  {t('datasources.importSqlSchema')}
                </Button>
                <Button size="small" type="primary" icon={<PlusOutlined />}
                  onClick={() => setAddTableOpen(true)}>
                  {t('datasources.addTable')}
                </Button>
              </Space>
            }
          />
          <Input
            prefix={<SearchOutlined />}
            placeholder={t('datasources.searchTable')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            style={{ marginBottom: 16 }}
            allowClear
          />
          <div style={{ maxHeight: embedded ? 480 : 'calc(100vh - 220px)', overflow: 'auto' }}>
            {filtered.length === 0 ? (
              <Empty description={t('datasources.noTables')} />
            ) : (
              filtered.map((table) => (
                <div key={table.table_name} style={{ marginBottom: 12, border: '1px solid var(--border-subtle)', borderRadius: 8, overflow: 'hidden' }}>
                  {/* Table header — click to expand/collapse */}
                  <div
                    style={{ padding: '8px 12px', background: 'var(--bg-elevated)', display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', userSelect: 'none' }}
                    onClick={() => toggleTable(table.table_name)}
                  >
                    {expandedTables.has(table.table_name)
                      ? <DownOutlined style={{ color: 'var(--text-muted)', fontSize: 11, flexShrink: 0 }} />
                      : <RightOutlined style={{ color: 'var(--text-muted)', fontSize: 11, flexShrink: 0 }} />
                    }
                    <TableOutlined style={{ color: '#1677ff', flexShrink: 0 }} />
                    <Text strong style={{ fontSize: 13 }}>{table.table_name}</Text>
                    <Tag style={{ fontSize: 11, marginLeft: 4 }}>{t('datasources.columnCount', { count: table.columns?.length ?? 0 })}</Tag>
                    {table.is_manual && <Tag color="gold" style={{ fontSize: 10 }}>{t('datasources.manual')}</Tag>}
                    {table.description && (
                      <Text type="secondary" style={{ fontSize: 11, marginLeft: 4 }}>— {table.description}</Text>
                    )}
                    <Space style={{ marginLeft: 'auto' }} onClick={e => e.stopPropagation()}>
                      <Button size="small" icon={<PlusOutlined />} onClick={() => openAddColumn(table.table_name)}>
                        {t('datasources.addColumn')}
                      </Button>
                      <Button size="small" icon={<EditOutlined />} onClick={() => openEditTable(table.table_name, table.description)} />
                      {onRefreshTable && !table.is_manual && (
                        <Tooltip title={t('datasources.refreshThisTable')}>
                          <Button
                            size="small"
                            icon={<ThunderboltOutlined />}
                            onClick={() => onRefreshTable(table.table_name)}
                          />
                        </Tooltip>
                      )}
                      <Popconfirm
                        title={t('datasources.deleteTableConfirm')}
                        onConfirm={() => deleteTableMutation.mutate(table.table_name)}
                      >
                        <Button size="small" danger icon={<DeleteOutlined />} />
                      </Popconfirm>
                    </Space>
                  </div>
                  {/* Columns — collapsible */}
                  {expandedTables.has(table.table_name) && (
                    <div style={{ borderTop: '1px solid var(--border-subtle)' }}>
                      {(table.columns ?? []).length > 0 ? (
                        <Table
                          size="small"
                          pagination={false}
                          dataSource={table.columns ?? []}
                          rowKey="name"
                      columns={[
                        { title: '#', key: 'idx', width: 40, render: (_: unknown, __: unknown, i: number) => i + 1 },
                        { title: t('datasources.columnName'), dataIndex: 'name', key: 'name', render: (v: string, record: { name: string; type: string; description?: string; nullable?: boolean; is_manual?: boolean }) => <Space><Text code style={{ fontSize: 12 }}>{v}</Text>{record.is_manual && <Tag color="gold" style={{ fontSize: 10 }}>{t('datasources.manual')}</Tag>}</Space> },
                        { title: t('datasources.columnType'), dataIndex: 'type', key: 'type', width: 140, render: (v: string) => <Tag>{v}</Tag> },
                        { title: t('datasources.nullable'), dataIndex: 'nullable', key: 'nullable', width: 80, align: 'center' as const, render: (v: boolean) => v ? <Tag color="default">{t('datasources.nullable')}</Tag> : <Tag color="purple">{t('datasources.notNull')}</Tag> },
                        { title: t('datasources.columnDesc'), dataIndex: 'description', key: 'description', render: (v?: string) => <Text style={{ fontSize: 12, color: v ? 'var(--text-primary)' : 'var(--text-muted)' }}>{v || '—'}</Text> },
                        {
                          title: '',
                          key: 'action',
                          width: 100,
                          render: (_: unknown, record: { name: string; type: string; description?: string; nullable?: boolean }) => (
                            <Space size={4}>
                              <Button size="small" icon={<EditOutlined />} onClick={() => openEditColumn(table.table_name, record)} />
                              <Popconfirm
                                title={t('datasources.deleteColumnConfirm')}
                                onConfirm={() => deleteColMutation.mutate({ table: table.table_name, column: record.name })}
                              >
                                <Button size="small" danger icon={<DeleteOutlined />} />
                              </Popconfirm>
                            </Space>
                          ),
                        },
                      ]}
                        />
                      ) : (
                        <div style={{ padding: '16px 12px', textAlign: 'center' }}>
                          <Text type="secondary" style={{ fontSize: 12 }}>{t('datasources.noColumns')}</Text>
                        </div>
                      )}
                    </div>
                  )}
            </div>
          ))
          )}
          </div>
          </>
        )}
    </>
  );

  const modals = (
    <>
      {/* Add Table Modal */}
      <Modal
        title={t('datasources.addTable')}
        open={addTableOpen}
        onCancel={() => { setAddTableOpen(false); addTableForm.resetFields(); }}
        footer={null}
        width={520}
      >
        <Form
          form={addTableForm}
          layout="vertical"
          onFinish={(values) => {
            const columns: ManualColumn[] = (values.columns as Array<{ col_name: string; col_type: string; col_desc?: string; col_nullable?: boolean }> ?? [])
              .filter(c => c.col_name?.trim())
              .map(c => ({
                name: c.col_name.trim(),
                type: c.col_type || 'VARCHAR(255)',
                description: c.col_desc,
                nullable: c.col_nullable,
              }));
            addTableMutation.mutate({ table_name: values.table_name, description: values.description, columns });
          }}
        >
          <Form.Item name="table_name" label={t('datasources.tableName')} rules={[{ required: true }]}>
            <Input placeholder={t('datasources.tablePlaceholder')} />
          </Form.Item>
          <Form.Item name="description" label={t('datasources.tableDescription')}>
            <TextArea rows={2} placeholder={t('datasources.tableDescriptionPlaceholder')} />
          </Form.Item>
          <Divider>{t('datasources.tabColumns')}</Divider>
          <Form.List name="columns">
            {(fields, { add, remove }) => (
              <>
                {fields.map(({ key, name }) => (
                  <Space key={key} style={{ display: 'flex', marginBottom: 8, alignItems: 'flex-start' }}>
                    <Form.Item name={[name, 'col_name']} rules={[{ required: true }]} style={{ marginBottom: 0 }}>
                      <Input placeholder={t('datasources.colNamePlaceholder')} style={{ width: 120 }} />
                    </Form.Item>
                    <Form.Item name={[name, 'col_type']} style={{ marginBottom: 0 }}>
                      <ColTypeSelect placeholder={t('datasources.selectType')} typeOptions={colTypeConfig.types} />
                    </Form.Item>
                    <Form.Item name={[name, 'col_desc']} style={{ marginBottom: 0 }}>
                      <Input placeholder={t('datasources.colDescPlaceholder')} style={{ width: 120 }} />
                    </Form.Item>
                    <Button size="small" danger icon={<DeleteOutlined />} onClick={() => remove(name)} />
                  </Space>
                ))}
                <Button type="dashed" block icon={<PlusOutlined />} onClick={() => add()}>
                  {t('datasources.addColumnLine')}
                </Button>
              </>
            )}
          </Form.List>
          <Form.Item style={{ marginTop: 16, marginBottom: 0 }}>
            <Button type="primary" htmlType="submit" loading={addTableMutation.isPending} block>
              {t('common.save')}
            </Button>
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={t('datasources.importSqlSchema')}
        open={importSqlOpen}
        onCancel={() => { setImportSqlOpen(false); importSqlForm.resetFields(); }}
        footer={null}
        width={760}
        destroyOnHidden
      >
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 16 }}
          message={t('datasources.importSqlSchemaTip')}
        />
        <Form
          form={importSqlForm}
          layout="vertical"
          initialValues={{ overwriteExisting: true }}
          onFinish={(values) => {
            importSqlMutation.mutate({
              sql: values.sql,
              overwriteExisting: values.overwriteExisting,
            });
          }}
        >
          <Form.Item
            name="sql"
            label={t('datasources.importSqlSchemaContent')}
            rules={[{ required: true, message: t('datasources.importSqlSchemaRequired') }]}
          >
            <TextArea
              rows={14}
              spellCheck={false}
              placeholder={t('datasources.importSqlSchemaPlaceholder')}
              style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace' }}
            />
          </Form.Item>
          <Form.Item name="overwriteExisting" valuePropName="checked">
            <Checkbox>{t('datasources.importSqlSchemaOverwrite')}</Checkbox>
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button
              type="primary"
              htmlType="submit"
              icon={<UploadOutlined />}
              loading={importSqlMutation.isPending}
              block
            >
              {t('datasources.importSqlSchemaSubmit')}
            </Button>
          </Form.Item>
        </Form>
      </Modal>

      {/* Edit Table Modal */}
      <Modal
        title={t('datasources.editTable')}
        open={editTableOpen}
        onCancel={() => { setEditTableOpen(false); editTableForm.resetFields(); }}
        footer={null}
        width={480}
      >
        <Form form={editTableForm} layout="vertical" onFinish={(values) => {
          putTableMutation.mutate({ table: editTargetTable, data: { table_name: values.table_name, description: values.description } });
        }}>
          <Form.Item name="table_name" label={t('datasources.tableName')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="description" label={t('datasources.tableDescription')}>
            <TextArea rows={3} />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button type="primary" htmlType="submit" loading={putTableMutation.isPending} block>
              {t('common.save')}
            </Button>
          </Form.Item>
        </Form>
      </Modal>

      {/* Add Column Modal */}
      <Modal
        title={`${t('datasources.addColumn')} — ${colTargetTable}`}
        open={addColOpen}
        onCancel={() => { setAddColOpen(false); addColForm.resetFields(); setColType(''); }}
        footer={null}
        width={480}
      >
        <Form form={addColForm} layout="vertical" onFinish={() => {
          if (!colType) {
            message.error(t('datasources.selectType'));
            return;
          }
          addColMutation.mutate({
            name: addColForm.getFieldValue('name'),
            type: colType,
            description: addColForm.getFieldValue('description'),
            nullable: addColForm.getFieldValue('nullable'),
          });
        }}>
          <Form.Item name="name" label={t('datasources.columnName')} rules={[{ required: true }]}>
            <Input placeholder={t('datasources.columnNamePlaceholder')} />
          </Form.Item>
          <Form.Item label={t('datasources.columnType')} required>
            <Select
              showSearch
              value={colType}
              onChange={(v) => setColType(v)}
              placeholder={t('datasources.selectType')}
              style={{ width: '100%' }}
              options={[
                ...colTypeConfig.types.map(typeOpt => ({ label: typeOpt, value: typeOpt })),
                { label: `— ${t('datasources.customTypeHint')}`, value: '__custom__', disabled: true },
              ]}
            />
          </Form.Item>
          <Form.Item name="nullable" label={t('datasources.nullable')} valuePropName="checked" initialValue={true}>
            <Switch checkedChildren={t('datasources.nullable')} unCheckedChildren={t('datasources.notNull')} />
          </Form.Item>
          <Form.Item name="description" label={t('datasources.columnDesc')}>
            <TextArea rows={2} />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button type="primary" htmlType="submit" loading={addColMutation.isPending} block>
              {t('common.save')}
            </Button>
          </Form.Item>
        </Form>
      </Modal>

      {/* Edit Column Modal */}
      <Modal
        title={`${t('datasources.editColumn')} — ${editColTarget.table}.${editColTarget.column}`}
        open={editColOpen}
        onCancel={() => { setEditColOpen(false); editColForm.resetFields(); setEditColType(''); }}
        footer={null}
        width={480}
      >
        <Form form={editColForm} layout="vertical" onFinish={() => {
          putColMutation.mutate({
            name: editColForm.getFieldValue('name'),
            type: editColType,
            description: editColForm.getFieldValue('description'),
            nullable: editColForm.getFieldValue('nullable'),
          });
        }}>
          <Form.Item name="name" label={t('datasources.columnName')} rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item label={t('datasources.columnType')} required>
            <Select
              showSearch
              value={editColType}
              onChange={(v) => setEditColType(v)}
              placeholder={t('datasources.selectType')}
              style={{ width: '100%' }}
              options={[
                ...colTypeConfig.types.map(typeOpt => ({ label: typeOpt, value: typeOpt })),
                { label: `— ${t('datasources.customTypeHint')}`, value: '__custom__', disabled: true },
              ]}
            />
          </Form.Item>
          <Form.Item name="nullable" label={t('datasources.nullable')} valuePropName="checked">
            <Switch checkedChildren={t('datasources.nullable')} unCheckedChildren={t('datasources.notNull')} />
          </Form.Item>
          <Form.Item name="description" label={t('datasources.columnDesc')}>
            <TextArea rows={2} />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button type="primary" htmlType="submit" loading={putColMutation.isPending} block>
              {t('common.save')}
            </Button>
          </Form.Item>
        </Form>
      </Modal>
    </>
  );

  if (embedded) {
    return (<>{body}{modals}</>);
  }

  return (
    <Drawer
      title={
        <Space>
          <SettingOutlined />
          {t('datasources.schemaManagement')}
          <Tag color="blue">{dataSource.name}</Tag>
        </Space>
      }
      placement="right"
      width={760}
      open
      onClose={onClose}
    >
      {body}
      {modals}
    </Drawer>
  );
}

// ─── Unified Schema Drawer (merges schema management + semantic index) ───────

interface UnifiedSchemaDrawerProps {
  dataSource: DataSourceInfo;
  onClose: () => void;
  /** Kick off a semantic refresh for a specific table. */
  onRefreshTable?: (tableName: string) => void;
  /** Trigger a schema re-discovery (shows DiscoverProgressModal in parent). */
  onRefetch?: () => void;
  isRefetching?: boolean;
  initialTab?: 'schema' | 'semantics';
  /**
   * Pre-populated schema tables from a discover response, so the schema tab
   * renders immediately without waiting for a secondary GET fetch. */
  eagerSchema?: DataSourceInfo['schema_info'];
}

function UnifiedSchemaDrawer({
  dataSource,
  onClose,
  onRefreshTable,
  onRefetch,
  isRefetching = false,
  initialTab = 'schema',
  eagerSchema,
}: UnifiedSchemaDrawerProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [tab, setTab] = useState<'schema' | 'semantics'>(initialTab);
  const onActiveTabRefresh = useCallback((key: string) => {
    if (key === 'schema') {
      qc.invalidateQueries({ queryKey: ['dataSources', 'detail', dataSource.id] });
      return;
    }
    qc.invalidateQueries({ queryKey: queryKeys.nl2sql.semantics(dataSource.id) });
    qc.invalidateQueries({ queryKey: ['nl2sql', 'table-semantics', dataSource.id] });
    qc.invalidateQueries({ queryKey: ['nl2sql', 'ds-semantics', dataSource.id] });
  }, [dataSource.id, qc]);
  const handleTabClick = useTabRefresh(tab, onActiveTabRefresh);

  return (
    <Drawer
      title={
        <Space>
          <DatabaseOutlined />
          {dataSource.name}
          <Tag color="blue">{DB_TYPE_SHORT[dataSource.db_type] ?? dataSource.db_type}</Tag>
        </Space>
      }
      placement="right"
      width={860}
      open
      onClose={onClose}
      extra={
        <Button
          type="primary"
          icon={<SyncOutlined spin={isRefetching} />}
          loading={isRefetching}
          onClick={onRefetch}
        >
          {t('datasources.refetchSchema')}
        </Button>
      }
    >
      <Tabs
        activeKey={tab}
        onChange={(key) => setTab(key as 'schema' | 'semantics')}
        onTabClick={handleTabClick}
        items={[
          {
            key: 'schema',
            label: (<span><SettingOutlined /> {t('datasources.schemaManagement')}</span>),
          },
          {
            key: 'semantics',
            label: (<span><ThunderboltOutlined /> {t('datasources.semanticIndex')}</span>),
          },
        ]}
      />
      <div style={{ marginTop: 12 }}>
        {tab === 'schema' && (
          <SchemaManagementDrawer
            dataSource={dataSource}
            onClose={onClose}
            onRefreshTable={onRefreshTable}
            isRefetching={isRefetching}
            embedded
          />
        )}
        {tab === 'semantics' && (
          <SemanticsDrawer
            dataSource={dataSource}
            onClose={onClose}
            embedded
          />
        )}
      </div>
    </Drawer>
  );
}

// ─── Content Component ────────────────────────────────────────────────────────

const DB_TYPE_OPTIONS: { value: DataSourceDbType; label: string }[] = [
  { value: 'mysql', label: 'MySQL' },
  { value: 'tidb', label: 'TiDB' },
  { value: 'postgres', label: 'PostgreSQL' },
  { value: 'clickhouse', label: 'ClickHouse' },
  { value: 'trino', label: 'Trino' },
  { value: 'presto', label: 'Presto' },
  { value: 'mongodb', label: 'MongoDB' },
];

const DB_TYPE_COLORS: Record<string, string> = {
  mysql: 'blue', tidb: 'cyan', postgres: 'geekblue', clickhouse: 'orange',
  presto: 'purple', trino: 'purple', mongodb: 'green',
  // Legacy types retained only to avoid `undefined` tag colours on pre-existing rows.
  hive: 'gold', http_api: 'default', mcp: 'default',
};

const DB_TYPE_SHORT: Record<string, string> = {
  mysql: 'MySQL', tidb: 'TiDB', postgres: 'PG', clickhouse: 'CH',
  presto: 'Presto', trino: 'Trino', mongodb: 'MongoDB',
  // Legacy types retained for display of pre-existing rows.
  hive: 'Hive', http_api: 'HTTP', mcp: 'MCP',
};

const isTrinoLikeType = (type?: string | null) => type === 'trino' || type === 'presto';

const normalizeHostInput = (raw: unknown) => {
  const value = String(raw ?? '').trim();
  const match = value.match(/^(https?):\/\/(.+)$/i);
  const secure = match ? match[1].toLowerCase() === 'https' : undefined;
  const rest = (match ? match[2] : value).replace(/^\/+/, '');
  const authority = rest.split('/')[0] ?? rest;
  const portMatch = authority.match(/^(.*):(\d+)$/);
  if (portMatch && !authority.startsWith('[')) {
    return {
      host: portMatch[1],
      port: Number(portMatch[2]) || undefined,
      secure,
    };
  }
  return { host: authority, port: undefined, secure };
};

const defaultPortForDbType = (type?: string | null) => {
  switch (type) {
    case 'postgres':
      return 5432;
    case 'clickhouse':
      return 8123;
    case 'trino':
    case 'presto':
      return 443;
    case 'mongodb':
      return 27017;
    case 'mysql':
    case 'tidb':
    default:
      return 3306;
  }
};

function DataSourcesContent() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const localEmbeddingNotice = useDismissibleNotice('aos.embedding-local-notice.v1');
  const [activeTab, setActiveTab] = useState<'tenant' | 'private'>('private');
  const onActiveTabRefresh = useCallback((key: string) => {
    if (key === 'tenant' || key === 'private') {
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.list() });
    }
  }, [qc]);
  const handleTabClick = useTabRefresh(activeTab, onActiveTabRefresh);
  const [modalOpen, setModalOpen] = useState(false);
  const [editDs, setEditDs] = useState<DataSourceInfo | null>(null);
  const [loadingEditId, setLoadingEditId] = useState<string | null>(null);
  const [form] = Form.useForm();
  const watchedDbType = Form.useWatch('type', form) as DataSourceDbType | undefined;
  const effectiveDbType = watchedDbType ?? editDs?.db_type ?? 'mysql';
  const isTrinoLike = isTrinoLikeType(effectiveDbType);
  const isMongoDb = effectiveDbType === 'mongodb';
  const [trinoSchemaOptions, setTrinoSchemaOptions] = useState<string[]>([]);
  const [isFetchingTrinoSchemas, setIsFetchingTrinoSchemas] = useState(false);

  // Keep form values in sync whenever editDs changes.
  // Ant Design Form caches initialValues by key — if the key prop changes (e.g. edit→create→edit)
  // the Form re-initializes automatically.  However, when editDs is updated to a new edit target
  // while the modal is already open, the Form's key may not yet reflect the new ID, so we
  // explicitly reset fields here to ensure the UI always shows the latest data.
  useEffect(() => {
    if (editDs) {
      const config = (editDs.config_plain ?? editDs.config_preview ?? {}) as Record<string, unknown>;
      const normalizedHost = normalizeHostInput(config.host);
      const port = typeof config.port === 'number'
        ? config.port
        : (normalizedHost.port ?? (Number(config.port) || defaultPortForDbType(editDs.db_type)));
      const schema = config.schema as string | undefined;
      const schemas = Array.isArray(config.schemas)
        ? (config.schemas as unknown[]).filter((v): v is string => typeof v === 'string' && v.trim().length > 0)
        : [];
      const effectiveSchemas = schemas.length > 0 ? schemas : (schema ? [schema] : []);
      const database = config.database as string | undefined;
      const password = typeof config.password === 'string' && config.password !== '[REDACTED]'
        ? config.password
        : undefined;
      form.setFieldsValue({
        name: editDs.name,
        description: editDs.description,
        type: editDs.db_type,
        host: normalizedHost.host,
        port,
        database: database ?? schema,
        catalog: config.catalog as string | undefined,
        schema: schema ?? database,
        schemas: effectiveSchemas,
        ssl: typeof config.ssl === 'boolean'
          ? config.ssl
          : (normalizedHost.secure ?? (isTrinoLikeType(editDs.db_type) && port === 443)),
        username: config.username as string | undefined,
        password: password ?? '',
        auth_source: config.auth_source as string | undefined,
        tls: typeof config.tls === 'boolean' ? config.tls : undefined,
        visibility: editDs.visibility,
        sensitive_columns: editDs.sensitive_columns ?? [],
      });
      setTrinoSchemaOptions(effectiveSchemas);
    } else {
      form.resetFields();
      setTrinoSchemaOptions([]);
    }
  }, [editDs, form]);

  const [testingId, setTestingId] = useState<string | null>(null);
  const [unifiedDrawerDs, setUnifiedDrawerDs] = useState<DataSourceInfo | null>(null);
  const [unifiedDrawerSchema, setUnifiedDrawerSchema] = useState<DataSourceInfo['schema_info'] | null>(null);
  const [unifiedDrawerTab, setUnifiedDrawerTab] = useState<'schema' | 'semantics'>('schema');

  // Discover progress modal state
  const [discoverProgressModal, setDiscoverProgressModal] = useState<{
    dataSourceId: string;
    taskId: string;
  } | null>(null);
  const [isDiscovering, setIsDiscovering] = useState(false);
  const [discoverModeModal, setDiscoverModeModal] = useState<null | { dataSourceId: string }>(null);
  const [backgroundTasks, setBackgroundTasks] = useState<Array<{ dataSourceId: string; taskId: string }>>([]);
  const [taskDrawerOpen, setTaskDrawerOpen] = useState(false);

  const { data, isLoading } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list({ per_page: 200 }),
    refetchInterval: 60_000,
  });
  const refreshTasksQuery = useQuery({
    queryKey: ['nl2sql', 'refresh-tasks'],
    queryFn: () => nl2sqlApi.listRefreshTasks({ limit: 50 }),
    refetchInterval: (query) => query.state.data?.items.some((task) =>
      task.status === 'pending' || task.status === 'running'
    ) ? 3000 : 15_000,
  });
  const refreshTasks = refreshTasksQuery.data?.items ?? [];
  const activeRefreshTaskCount = refreshTasks.filter((task) =>
    task.status === 'pending' || task.status === 'running'
  ).length;

  // Fetch the current tenant's embedding model config to know if semantic indexing is available.
  const { data: embeddingConfig } = useQuery({
    queryKey: queryKeys.nl2sql.embeddingConfig(),
    queryFn: () => nl2sqlApi.getEmbeddingConfig(),
    staleTime: 60_000,
  });
  const { data: embeddingHealth } = useQuery({
    queryKey: queryKeys.nl2sql.embeddingHealth(),
    queryFn: () => nl2sqlApi.getEmbeddingHealth(),
    staleTime: 10_000,
    refetchInterval: 15_000,
  });
  // Keep the Unified Schema Drawer's dataSource in sync with refreshed list
  // data, so the drawer always shows up-to-date schema_info and description.
  useEffect(() => {
    if (unifiedDrawerDs && data?.data_sources) {
      const refreshed = data.data_sources.find((ds) => ds.id === unifiedDrawerDs.id);
      if (refreshed) {
        const prevJson = JSON.stringify(unifiedDrawerDs);
        const nextJson = JSON.stringify(refreshed);
        if (prevJson !== nextJson) {
          setUnifiedDrawerDs(refreshed);
        }
      }
    }
  }, [data, unifiedDrawerDs]);

  const tenantSources = data?.data_sources.filter((ds) => ds.visibility === 'tenant') ?? [];
  const privateSources = data?.data_sources.filter((ds) => ds.visibility === 'private') ?? [];
  const visibleSources = activeTab === 'tenant' ? tenantSources : privateSources;

  const createMutation = useMutation({
    mutationFn: async (values: Record<string, unknown>) => {
      const newDs = await dataSourcesApi.create(values as Parameters<typeof dataSourcesApi.create>[0]);
      return newDs;
    },
    onSuccess: async (newDs: DataSourceInfo) => {
      message.success(t('common.operateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
      setModalOpen(false);
      setEditDs(null);
      form.resetFields();
      // Auto-discover schema for the newly created data source
      try {
        const result = await dataSourcesApi.discoverSchema(newDs.id) as {
          refresh_task_id?: string | null;
        };
        const taskId = result?.refresh_task_id ?? null;
        if (taskId) {
          setDiscoverProgressModal({ dataSourceId: newDs.id, taskId });
        } else {
          message.info(t('datasources.schemaDiscovered'));
        }
      } catch (err) {
        message.warning(err instanceof ApiError ? err.message : t('datasources.autoDiscoverFailed'));
      }
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
    },
  });

  const updateMutation = useMutation({
    mutationFn: async ({ id, ...data }: { id: string; data: Record<string, unknown> }) => {
      return dataSourcesApi.update(id, data as Parameters<typeof dataSourcesApi.update>[1]);
    },
    onSuccess: async (updatedDs: DataSourceInfo) => {
      message.success(t('common.operateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
      setModalOpen(false);
      setEditDs(null);
      form.resetFields();
      // Auto-discover schema if the connection config changed
      try {
        const result = await dataSourcesApi.discoverSchema(updatedDs.id) as {
          refresh_task_id?: string | null;
        };
        const taskId = result?.refresh_task_id ?? null;
        if (taskId) {
          setDiscoverProgressModal({ dataSourceId: updatedDs.id, taskId });
        }
      } catch (err) {
        message.warning(err instanceof ApiError ? err.message : t('datasources.autoDiscoverFailed'));
      }
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => dataSourcesApi.delete(id),
    onSuccess: () => {
      message.success(t('common.operateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
    },
    onError: (err: unknown) => {
      if (err instanceof ApiError) message.error(err.message);
    },
  });

  const testMutation = useMutation({
    mutationFn: (id: string) => dataSourcesApi.testConnection(id),
    onSuccess: (result, id) => {
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
      setTestingId(null);
      if (result.success) {
        message.success(t('datasources.testSuccess'));
      } else {
        message.error(result.error ?? t('datasources.testFailed'));
      }
    },
    onError: (err: unknown, id) => {
      setTestingId(null);
      if (err instanceof ApiError) message.error(err.message);
    },
  });

  const handleFetchTrinoSchemas = async () => {
    try {
      const values = await form.validateFields(['host', 'catalog', 'username']);
      const normalizedHost = normalizeHostInput(values.host);
      const dbType = (form.getFieldValue('type') as string) || effectiveDbType;
      const effectivePort = normalizedHost.port ?? (Number(form.getFieldValue('port')) || defaultPortForDbType(dbType));
      setIsFetchingTrinoSchemas(true);
      const result = await dataSourcesApi.discoverTrinoSchemas({
        host: normalizedHost.host,
        port: effectivePort,
        catalog: String(values.catalog ?? '').trim(),
        username: String(values.username ?? '').trim(),
        password: String(form.getFieldValue('password') ?? ''),
        ssl: form.getFieldValue('ssl') !== false || normalizedHost.secure === true,
        basic_auth: true,
      });
      const schemas = Array.from(new Set((result.schemas ?? []).filter(Boolean)));
      setTrinoSchemaOptions(schemas);
      const current = form.getFieldValue('schemas');
      const currentList = Array.isArray(current) ? current.filter(Boolean) : [];
      const retained = currentList.filter((schema) => schemas.includes(schema));
      if (retained.length > 0) {
        form.setFieldsValue({ schemas: retained, schema: retained[0] });
      } else if (schemas.length === 1) {
        form.setFieldsValue({ schemas, schema: schemas[0] });
      }
      if (schemas.length === 0) {
        message.warning(t('datasources.trinoSchemasEmpty'));
      } else {
        message.success(t('datasources.trinoSchemasFetched', { count: schemas.length }));
      }
    } catch (err) {
      if (err instanceof ApiError) {
        message.error(err.message);
      } else if (err instanceof Error) {
        message.error(err.message);
      }
    } finally {
      setIsFetchingTrinoSchemas(false);
    }
  };

  const handleSelectAllTrinoSchemas = () => {
    if (trinoSchemaOptions.length === 0) return;
    form.setFieldsValue({
      schemas: trinoSchemaOptions,
      schema: trinoSchemaOptions[0],
    });
  };

  const handleFormSubmit = (values: Record<string, unknown>) => {
    const dbType = values.type as string;
    const trinoLike = isTrinoLikeType(dbType);
    const mongoDb = dbType === 'mongodb';
    const normalizedHost = normalizeHostInput(values.host);
    const effectivePort = normalizedHost.port ?? (Number(values.port) || defaultPortForDbType(dbType));
    const pwd = typeof values.password === 'string' ? values.password : '';
    const selectedSchemas = Array.isArray(values.schemas)
      ? (values.schemas as unknown[]).filter((v): v is string => typeof v === 'string' && v.trim().length > 0)
      : [];
    const fallbackSchema = typeof values.schema === 'string' && values.schema.trim()
      ? values.schema.trim()
      : '';
    const trinoSchemas = selectedSchemas.length > 0 ? selectedSchemas : (fallbackSchema ? [fallbackSchema] : []);
    const config: Record<string, unknown> = trinoLike
      ? {
          host: normalizedHost.host,
          port: effectivePort || 443,
          catalog: values.catalog as string,
          schema: trinoSchemas[0] ?? '',
          schemas: trinoSchemas,
          username: values.username as string,
          ssl: values.ssl !== false || normalizedHost.secure === true,
          basic_auth: true,
        }
      : mongoDb
        ? {
            host: normalizedHost.host,
            port: effectivePort || 27017,
            database: values.database as string,
            username: String(values.username ?? '').trim(),
            auth_source: String(values.auth_source ?? '').trim() || undefined,
            tls: values.tls === true,
          }
        : {
          host: normalizedHost.host,
          port: effectivePort,
          database: values.database as string,
          username: values.username as string,
        };
    // Create always sends the field. Edit sends it once the detail endpoint has
    // supplied config_plain, so clearing the input intentionally saves an empty
    // password instead of being interpreted as "keep existing".
    if (!editDs || editDs.config_plain || form.isFieldTouched('password')) {
      config.password = pwd;
    }
    if (mongoDb && editDs) {
      const existingConfig = (editDs.config_plain ?? {}) as Record<string, unknown>;
      const existingUri = typeof existingConfig.uri === 'string'
        && existingConfig.uri !== '[REDACTED]'
        ? existingConfig.uri.trim()
        : '';
      if (existingUri) config.uri = existingUri;
    }

    if (editDs) {
      // Update payload — no 'type' field (db_type is immutable) and no 'visibility'
      // in the top-level payload (changing visibility requires a separate API call).
      const payload = {
        name: values.name as string,
        description: values.description as string | undefined,
        config,
        sensitive_columns: values.sensitive_columns ?? [],
      };
      updateMutation.mutate({ id: editDs.id, data: payload });
    } else {
      const payload = {
        name: values.name as string,
        description: values.description as string | undefined,
        type: dbType,
        visibility: activeTab,
        config,
        sensitive_columns: values.sensitive_columns ?? [],
      };
      createMutation.mutate(payload);
    }
  };

  const openEdit = async (ds: DataSourceInfo) => {
    setEditDs(ds);
    setModalOpen(true);
    setLoadingEditId(ds.id);
    try {
      const detail = await dataSourcesApi.get(ds.id);
      setEditDs(detail);
    } catch (err) {
      if (err instanceof ApiError) {
        message.error(err.message);
      }
    } finally {
      setLoadingEditId(null);
    }
  };

  const openCreate = () => {
    setEditDs(null);
    setTrinoSchemaOptions([]);
    setModalOpen(true);
  };

  const columns: ColumnsType<DataSourceInfo> = [
    {
      title: t('datasources.tableColumns.name'),
      dataIndex: 'name',
      key: 'name',
      width: 240,
      render: (name: string, record) => (
        <div>
          <Space>
            <Tag color={DB_TYPE_COLORS[record.db_type] ?? 'default'}>
              {DB_TYPE_SHORT[record.db_type] ?? record.db_type}
            </Tag>
            <Text strong style={{ fontSize: 13 }}>{name}</Text>
          </Space>
          {record.description && (
            <div>
              <Text type="secondary" style={{ fontSize: 11 }}>{record.description}</Text>
            </div>
          )}
        </div>
      ),
    },
    {
      title: t('datasources.columns.visibility'),
      dataIndex: 'visibility',
      key: 'visibility',
      width: 90,
      render: (v: string) => (
        <Tag color={v === 'tenant' ? 'purple' : 'default'}>
          {v === 'tenant' ? t('datasources.tenantShared') : t('datasources.private')}
        </Tag>
      ),
    },
    {
      title: t('datasources.status'),
      key: 'status',
      width: 120,
      render: (_, record) => {
        if (record.last_error) {
          return (
            <Space direction="vertical" size={0}>
              <Tag color="error">{t('datasources.statuses.error')}</Tag>
              <Text type="danger" style={{ fontSize: 11 }} ellipsis>
                {record.last_error.slice(0, 40)}
              </Text>
            </Space>
          );
        }
        if (!record.last_tested_at) {
          return <Tag>{t('datasources.statuses.untested')}</Tag>;
        }
        return (
          <Space direction="vertical" size={0}>
            <Tag color="success">{t('datasources.statuses.connected')}</Tag>
            <Text type="secondary" style={{ fontSize: 11 }}>
              {new Date(record.last_tested_at).toLocaleString('zh-CN')}
            </Text>
          </Space>
        );
      },
    },
    {
      title: t('datasources.tables'),
      dataIndex: 'schema_info',
      key: 'schema_info',
      width: 80,
      render: (schema: { tables: DataSourceSchemaInfo[]; foreign_keys?: unknown[] } | DataSourceSchemaInfo[] | null) => {
        const tables = Array.isArray(schema)
          ? schema
          : schema?.tables;
        if (!tables?.length) return <span style={{ color: 'var(--text-muted)', fontSize: 12 }}>—</span>;
        return <Tag>{tables.length}</Tag>;
      },
    },
    {
      title: t('common.actions'),
      key: 'action',
      width: 260,
      render: (_, record) => {
        const hasDiscoverModal = !!discoverProgressModal;
        return (
        <Space size={4}>
          <Tooltip title={!embeddingConfig?.available ? t('datasources.noEmbeddingModel') : undefined}>
            <Button
              size="small"
              icon={<PlayCircleOutlined />}
              loading={testingId === record.id}
              disabled={hasDiscoverModal || !embeddingConfig?.available}
              onClick={() => { setTestingId(record.id); testMutation.mutate(record.id); }}
            >
              {t('datasources.test')}
            </Button>
          </Tooltip>
          <Tooltip title={!embeddingConfig?.available ? t('datasources.noEmbeddingModel') : undefined}>
            <Button
              size="small"
              icon={<DatabaseOutlined />}
              disabled={hasDiscoverModal || !embeddingConfig?.available}
              onClick={() => { setUnifiedDrawerDs(record); setUnifiedDrawerSchema(null); setUnifiedDrawerTab('schema'); }}
            >
              {t('datasources.schema')}
            </Button>
          </Tooltip>
          <Button
            size="small"
            icon={<EditOutlined />}
            disabled={hasDiscoverModal}
            loading={loadingEditId === record.id}
            onClick={() => openEdit(record)}
          >
            {t('common.edit')}
          </Button>
          <Popconfirm
            title={t('datasources.deleteConfirm')}
            onConfirm={() => deleteMutation.mutate(record.id)}
          >
            <Button size="small" danger icon={<DeleteOutlined />} disabled={hasDiscoverModal}>
              {t('common.delete')}
            </Button>
          </Popconfirm>
        </Space>
        );
      },
    },
  ];

  const renderConfigForm = () => {
    // All supported db_types share the same connection shape. We keep
    // the `type` form field so users can still pick MySQL vs Postgres
    // (needed by the backend driver selection), but stopped switching
    // config UIs per-type once we removed http_api / mcp.
    return (
      <>
        <Form.Item name="name" label={t('datasources.name')} rules={[{ required: true }]}>
          <Input placeholder={t('datasources.namePlaceholder')} />
        </Form.Item>
        <Form.Item name="type" label={t('datasources.type')} rules={[{ required: true }]} initialValue="mysql">
          {editDs ? (
            <Select options={DB_TYPE_OPTIONS.map((o) => ({ value: o.value, label: o.label }))} disabled />
          ) : (
            <Select
              options={DB_TYPE_OPTIONS.map((o) => ({ value: o.value, label: o.label }))}
              onChange={(value) => {
                const nextPort = defaultPortForDbType(value);
                form.setFieldsValue({ port: nextPort });
                if (isTrinoLikeType(value)) {
                  form.setFieldsValue({ ssl: true });
                } else {
                  form.setFieldsValue({ schemas: [], schema: undefined });
                  setTrinoSchemaOptions([]);
                }
              }}
            />
          )}
        </Form.Item>
        {editDs && (
          <Form.Item name="visibility" label={t('datasources.columns.visibility')}>
            <Select
              options={[
                { value: 'private', label: t('datasources.private') },
                { value: 'tenant', label: t('datasources.tenantShared') },
              ]}
              disabled
            />
          </Form.Item>
        )}
        <>
          <Form.Item
            name="host"
            label={t('datasources.host')}
            rules={[
              {
                validator: (_, value) => {
                  return String(value ?? '').trim()
                    ? Promise.resolve()
                    : Promise.reject(new Error(t('datasources.hostRequired')));
                },
              },
            ]}
          >
            <Input placeholder={t('datasources.hostPlaceholder')} />
          </Form.Item>
          <Form.Item name="port" label={t('datasources.port')} initialValue={3306}>
            <Input type="number" placeholder={t('datasources.portPlaceholder')} />
          </Form.Item>
          {isTrinoLike ? (
            <>
              <Form.Item label={t('datasources.catalog')} required>
                <Space.Compact block>
                  <Form.Item name="catalog" noStyle rules={[{ required: true }]}>
                    <Input placeholder={t('datasources.catalogPlaceholder')} />
                  </Form.Item>
                  <Button
                    icon={<SearchOutlined />}
                    loading={isFetchingTrinoSchemas}
                    onClick={handleFetchTrinoSchemas}
                  >
                    {t('datasources.fetchSchemas')}
                  </Button>
                </Space.Compact>
              </Form.Item>
              <Form.Item
                label={t('datasources.schemaName')}
                required
                extra={t('datasources.trinoSchemasHint')}
              >
                <Space.Compact block>
                  <Form.Item
                    name="schemas"
                    noStyle
                    rules={[
                      {
                        validator: (_, value) => {
                          if (Array.isArray(value) && value.length > 0) return Promise.resolve();
                          return Promise.reject(new Error(t('datasources.schemaRequired')));
                        },
                      },
                    ]}
                  >
                    <Select
                      mode="multiple"
                      showSearch
                      allowClear
                      placeholder={t('datasources.schemaPlaceholder')}
                      options={Array.from(new Set([
                        ...trinoSchemaOptions,
                        ...((form.getFieldValue('schemas') as string[] | undefined) ?? []),
                      ])).filter(Boolean).map((schema) => ({ label: schema, value: schema }))}
                      style={{ width: '100%' }}
                    />
                  </Form.Item>
                  <Button
                    icon={<CheckCircleFilled />}
                    disabled={trinoSchemaOptions.length === 0}
                    onClick={handleSelectAllTrinoSchemas}
                  >
                    {t('datasources.selectAllSchemas')}
                  </Button>
                </Space.Compact>
              </Form.Item>
              <Form.Item
                name="ssl"
                label={t('datasources.useHttps')}
                valuePropName="checked"
                initialValue={true}
              >
                <Switch />
              </Form.Item>
            </>
          ) : (
            <Form.Item name="database" label={t('datasources.databaseName')} rules={[{ required: true }]}>
              <Input placeholder={t('datasources.dbNamePlaceholder')} />
            </Form.Item>
          )}
          {isMongoDb && (
            <>
              <Form.Item name="auth_source" label={t('datasources.mongodbAuthSource')}>
                <Input placeholder="admin" />
              </Form.Item>
              <Form.Item name="tls" label={t('datasources.mongodbTls')} valuePropName="checked">
                <Switch />
              </Form.Item>
            </>
          )}
          <Form.Item name="username" label={t('datasources.username')} rules={[{ required: !isMongoDb }]}>
            <Input placeholder={t('datasources.usernamePlaceholder')} />
          </Form.Item>
          <Form.Item
            name="password"
            label={t('datasources.password')}
            rules={[{ required: !editDs && !isTrinoLike && !isMongoDb }]}
            extra={isTrinoLike
              ? t('datasources.trinoPasswordHint')
              : (isMongoDb ? t('datasources.mongodbCredentialsHint') : undefined)}
          >
            <Input.Password placeholder={editDs ? t('datasources.passwordEditPlaceholder') : t('datasources.passwordPlaceholder')} />
          </Form.Item>
        </>
        <Form.Item name="description" label={t('datasources.description')}>
          <TextArea rows={2} placeholder={t('datasources.descriptionPlaceholder')} />
        </Form.Item>
        <Form.Item
          name="sensitive_columns"
          label={t('datasources.sensitiveColumns')}
          extra={t('datasources.sensitiveColumnsHint')}
        >
          <Select
            mode="tags"
            placeholder={t('datasources.sensitiveColumnsPlaceholder')}
            tokenSeparators={[',', ';', ' ']}
            options={[]}
          />
        </Form.Item>
      </>
    );
  };

  return (
    <div style={{ padding: '24px 24px 0' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 20 }}>
        <div>
          <Title level={3} style={{ margin: 0 }}>{t('datasources.title')}</Title>
          <Text type="secondary" style={{ fontSize: 12 }}>
            {t('datasources.subtitle')}
          </Text>
        </div>
        <Space>
          <Badge count={activeRefreshTaskCount} size="small">
            <Button icon={<ClockCircleOutlined />} onClick={() => setTaskDrawerOpen(true)}>
              {t('datasources.indexTasks')}
            </Button>
          </Badge>
          <Tooltip title={!embeddingConfig?.available ? t('datasources.noEmbeddingModel') : undefined}>
            <Button type="primary" icon={<PlusOutlined />} onClick={openCreate} disabled={!embeddingConfig?.available}>
              {t('datasources.add')}
            </Button>
          </Tooltip>
        </Space>
      </div>

      {/* Local embeddings work out of the box; a tenant API is an optional quality upgrade. */}
      {embeddingConfig?.configured_via === 'local' && localEmbeddingNotice.visible && (
        <Alert
          type="info"
          showIcon
          closable
          onClose={localEmbeddingNotice.dismiss}
          message={t('apikeys.noKeyWarning')}
          description={t('apikeys.noKeyWarningDesc')}
          action={
            <Button size="small" type="primary" onClick={() => navigate('/keys')}>
              {t('apikeys.configureEmbeddingEnhancement')}
            </Button>
          }
          style={{ marginBottom: 16 }}
        />
      )}
      {embeddingHealth && (
        <Alert
          type={embeddingHealth.ann?.state === 'loaded' ? 'success' : 'info'}
          showIcon
          message={embeddingHealth.ann?.state === 'loaded'
            ? t('datasources.annReady')
            : t('datasources.annUnavailable')}
          description={[
            embeddingHealth.ann?.reason,
            embeddingHealth.ann?.snapshot_pending ? t('datasources.annSnapshotPending') : null,
            embeddingHealth.ann?.state === 'loaded' ? t('datasources.annReadyDesc') : t('datasources.annUnavailableDesc'),
          ].filter(Boolean).join(' · ')}
          style={{ marginBottom: 16 }}
        />
      )}

      <Tabs
        activeKey={activeTab}
        onChange={(key) => setActiveTab(key as 'tenant' | 'private')}
        onTabClick={handleTabClick}
        style={{ marginBottom: 16 }}
        items={[
          {
            key: 'private',
            label: `${t('datasources.myDataSources')} (${privateSources.length})`,
            children: null,
          },
          {
            key: 'tenant',
            label: `${t('datasources.tenantDataSources')} (${tenantSources.length})`,
            children: null,
          },
        ]}
      />

      <Card>
        {isLoading ? (
          <div style={{ textAlign: 'center', padding: 40 }}><Spin size="large" /></div>
        ) : visibleSources.length > 0 ? (
          <Table
            columns={columns}
            dataSource={visibleSources}
            rowKey="id"
            pagination={{ pageSize: 20, size: 'small' }}
            size="small"
            scroll={{ x: 'max-content' }}
          />
        ) : (
          <Empty description={t('datasources.empty.title')} />
        )}
      </Card>

      <Drawer
        title={t('datasources.indexTasks')}
        open={taskDrawerOpen}
        onClose={() => setTaskDrawerOpen(false)}
        width={520}
        extra={<Button icon={<SyncOutlined />} onClick={() => refreshTasksQuery.refetch()} loading={refreshTasksQuery.isFetching} />}
      >
        <List
          loading={refreshTasksQuery.isLoading}
          dataSource={refreshTasks}
          locale={{ emptyText: t('datasources.indexTasksEmpty') }}
          renderItem={(task) => {
            const active = task.status === 'pending' || task.status === 'running';
            const statusColor = task.status === 'completed' ? 'success'
              : task.status === 'failed' ? 'error'
                : 'processing';
            return (
              <List.Item
                actions={[
                  <Button
                    key="view"
                    type="link"
                    size="small"
                    onClick={() => {
                      setDiscoverProgressModal({ dataSourceId: task.datasource_id, taskId: task.task_id });
                      setTaskDrawerOpen(false);
                    }}
                  >
                    {t('datasources.viewProgress')}
                  </Button>,
                ]}
              >
                <List.Item.Meta
                  title={
                    <Space wrap>
                      <Text strong>{task.datasource_name}</Text>
                      <Tag color={statusColor}>{t(`datasources.indexTaskStatus.${task.status}`, { defaultValue: task.status })}</Tag>
                    </Space>
                  }
                  description={
                    <Space direction="vertical" size={4} style={{ width: '100%' }}>
                      <Text type="secondary" style={{ fontSize: 12 }}>
                        {t('datasources.indexTaskProcessed', {
                          processed: task.processed_tables,
                          total: task.total_tables,
                        })}
                      </Text>
                      <Progress percent={task.progress} size="small" status={task.status === 'failed' ? 'exception' : active ? 'active' : 'normal'} />
                      {task.error_message && <Text type="danger" style={{ fontSize: 12 }}>{task.error_message}</Text>}
                    </Space>
                  }
                />
              </List.Item>
            );
          }}
        />
      </Drawer>

      {/* Create/Edit Modal */}
      <Modal
        title={editDs ? t('datasources.edit') : t('datasources.addDataSource')}
        open={modalOpen}
        onCancel={() => { setModalOpen(false); setEditDs(null); setTrinoSchemaOptions([]); form.resetFields(); }}
        footer={null}
        width={640}
        destroyOnHidden
      >
        {activeTab === 'tenant' && !editDs && (
          <Alert
            message={t('datasources.tenantNotice')}
            type="info"
            showIcon
            style={{ marginBottom: 16 }}
          />
        )}
        <Form
          form={form}
          layout="vertical"
          key={editDs?.id ?? 'create'}
          onFinish={handleFormSubmit}
        >
          {renderConfigForm()}
          <Form.Item style={{ marginBottom: 0, marginTop: 8 }}>
            <Space>
              <Button
                htmlType="submit"
                type="primary"
                loading={createMutation.isPending || updateMutation.isPending}
                disabled={!!editDs && loadingEditId === editDs.id}
              >
                {t('common.save')}
              </Button>
              <Button onClick={() => { setModalOpen(false); setEditDs(null); form.resetFields(); }}>
                {t('common.cancel')}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>

      {/* Unified Schema Drawer (structure + semantics tabs) */}
      {unifiedDrawerDs && (
        <UnifiedSchemaDrawer
          dataSource={unifiedDrawerDs}
          onClose={() => { setUnifiedDrawerDs(null); setUnifiedDrawerSchema(null); }}
          initialTab={unifiedDrawerTab}
          eagerSchema={unifiedDrawerSchema}
          onRefreshTable={(tableName) => {
            if (isDiscovering) return;
            setIsDiscovering(true);
            dataSourcesApi.discoverTableSchema(unifiedDrawerDs.id, tableName).then((result) => {
              if (result.refresh_task_id) {
                setDiscoverProgressModal({ dataSourceId: unifiedDrawerDs.id, taskId: result.refresh_task_id });
                message.info(t('datasources.singleTableRefreshStarted', { table: tableName }) ?? `Refreshing ${tableName}…`);
              } else {
                message.info(t('datasources.tableSchemaUnchanged', { table: tableName }));
              }
            }).catch((err) => {
              if (err instanceof ApiError) message.error(err.message);
            }).finally(() => setIsDiscovering(false));
          }}
          onRefetch={() => {
            if (isDiscovering) return;
            setDiscoverModeModal({ dataSourceId: unifiedDrawerDs.id });
          }}
          isRefetching={isDiscovering || !!discoverProgressModal}
        />
      )}

      <Modal
        open={!!discoverModeModal}
        title={t('datasources.discoverModeTitle')}
        onCancel={() => setDiscoverModeModal(null)}
        footer={null}
        destroyOnHidden
      >
        <Space direction="vertical" style={{ width: '100%' }}>
          <Button
            block
            type="primary"
            onClick={() => {
              const target = discoverModeModal;
              if (!target || isDiscovering) return;
              setIsDiscovering(true);
              setDiscoverModeModal(null);
              dataSourcesApi.discoverSchema(target.dataSourceId, 'incremental').then((result) => {
                if (result.refresh_task_id) {
                  setDiscoverProgressModal({ dataSourceId: target.dataSourceId, taskId: result.refresh_task_id });
                } else {
                  message.success(t('datasources.schemaDiscovered'));
                  qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
                }
              }).catch((err) => {
                if (err instanceof ApiError) message.error(err.message);
              }).finally(() => setIsDiscovering(false));
            }}
          >
            {t('datasources.discoverModeIncremental')}
          </Button>
          <Button
            block
            onClick={() => {
              const target = discoverModeModal;
              if (!target || isDiscovering) return;
              setIsDiscovering(true);
              setDiscoverModeModal(null);
              dataSourcesApi.discoverSchema(target.dataSourceId, 'force').then((result) => {
                if (result.refresh_task_id) {
                  setDiscoverProgressModal({ dataSourceId: target.dataSourceId, taskId: result.refresh_task_id });
                } else {
                  message.success(t('datasources.schemaDiscovered'));
                  qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
                }
              }).catch((err) => {
                if (err instanceof ApiError) message.error(err.message);
              }).finally(() => setIsDiscovering(false));
            }}
          >
            {t('datasources.discoverModeForce')}
          </Button>
        </Space>
      </Modal>

      {/* Auto-discover progress modal after create/update */}
      {discoverProgressModal && (
        <DiscoverProgressModal
          dataSourceId={discoverProgressModal.dataSourceId}
          taskId={discoverProgressModal.taskId}
          onDone={(success, failedTables) => {
            setDiscoverProgressModal(null);
            if (!success && failedTables === undefined && discoverProgressModal) {
              setBackgroundTasks(prev => [...prev, {
                dataSourceId: discoverProgressModal.dataSourceId,
                taskId: discoverProgressModal.taskId,
              }]);
              return;
            }
            qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
            if (success && failedTables && failedTables.length > 0) {
              message.warning(t('datasources.schemaDiscoveredWithSkips', { count: failedTables.length }));
            } else if (success) {
              message.success(t('datasources.schemaRefreshCompleted'));
            }
          }}
          onViewSchema={(dataSourceId) => {
            setDiscoverProgressModal(null);
            const ds = data?.data_sources.find((d) => d.id === dataSourceId) ?? unifiedDrawerDs ?? null;
            if (ds) {
              setUnifiedDrawerDs(ds);
              setUnifiedDrawerSchema(null);
              setUnifiedDrawerTab('schema');
            }
          }}
        />
      )}
      {backgroundTasks.map(bt => (
        <BackgroundTaskPoller
          key={bt.taskId}
          taskId={bt.taskId}
          dataSourceId={bt.dataSourceId}
          onComplete={(tid) => setBackgroundTasks(prev => prev.filter(t => t.taskId !== tid))}
        />
      ))}
    </div>
  );
}

function BackgroundTaskPoller({ taskId, onComplete }: { taskId: string; dataSourceId: string; onComplete: (taskId: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const { data } = useQuery({
    queryKey: ['nl2sql', 'refresh-task-bg', taskId],
    queryFn: () => nl2sqlApi.getRefreshTaskStatus(taskId),
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      if (status === 'completed' || status === 'failed') return false;
      return 3000;
    },
  });

  useEffect(() => {
    if (data?.status === 'completed') {
      message.success(t('datasources.schemaRefreshCompleted'));
      qc.invalidateQueries({ queryKey: queryKeys.dataSources.all() });
      onComplete(taskId);
    } else if (data?.status === 'failed') {
      message.error(data?.error_message || t('datasources.discoverPhaseFailed'));
      onComplete(taskId);
    }
  }, [data?.status]);

  return null;
}

export default function DataSourcesPage() {
  return (
    <ErrorBoundary>
      <DataSourcesContent />
    </ErrorBoundary>
  );
}
