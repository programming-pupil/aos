import { useState } from 'react';
import {
  Form, Table, Button, Space, Modal, Input, Select, Tag, Typography,
  Divider, message, Popconfirm, Tooltip, Empty, Spin, Alert, Drawer, Switch,
} from 'antd';
import {
  PlusOutlined, EditOutlined, DeleteOutlined,
  TableOutlined, SearchOutlined, SettingOutlined,
  DownOutlined, RightOutlined, ThunderboltOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { dataSourcesApi } from '@/api';
import { ApiError } from '@/api/errors';
import { useTranslation } from 'react-i18next';
import type { DataSourceInfo, DataSourceSchemaInfo, ManualColumn } from '@/types';

const DB_TYPES: Record<string, { label: string; types: string[] }> = {
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

interface SchemaManagementDrawerProps {
  dataSource: DataSourceInfo;
  onClose: () => void;
  onRefresh: () => void;
  /** Kick off a semantic refresh for a single table. Used by the
   *  per-table "refresh index" button so users don't have to re-index
   *  the whole datasource after editing one description. */
  onRefreshTable?: (tableName: string) => void;
  /** When true, render the inner panes without the outer Drawer wrapper. */
  embedded?: boolean;
  /**
   * Pre-populated schema tables to render immediately, bypassing the async
   * `dataSourcesApi.get()` fetch. Used by the discover-flow: the
   * discover response already contains the full schema, so we pass it here
   * to avoid a blank drawer while the fetch resolves. The drawer still
   * re-fetches in the background to pick up any other field changes
   * (name, description, etc.). */
  eagerSchema?: DataSourceInfo['schema_info'];
}

function SchemaManagementDrawer({
  dataSource,
  onClose,
  onRefresh,
  onRefreshTable,
  embedded = false,
  eagerSchema,
}: SchemaManagementDrawerProps) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [search, setSearch] = useState('');
  const [expandedTables, setExpandedTables] = useState<Set<string>>(new Set());

  // Load current schema (still needed for name/description/etc. field updates).
  const { data: dsDetail, isLoading } = useQuery({
    queryKey: ['dataSources', 'detail', dataSource.id],
    queryFn: () => dataSourcesApi.get(dataSource.id),
    staleTime: 30_000,
  });

  // eagerSchema (from discover) is always a flat array. dsDetail.schema_info
  // may be a flat array (manual tables only) OR a nested object
  // { tables, foreign_keys } (after discover has run). Normalise to an array.
  const normalizeSchema = (v: unknown): DataSourceSchemaInfo[] | null => {
    if (Array.isArray(v)) return v as DataSourceSchemaInfo[];
    if (v && typeof v === 'object' && 'tables' in (v as Record<string, unknown>)) {
      return ((v as Record<string, unknown>).tables) as DataSourceSchemaInfo[];
    }
    return null;
  };

  const schemas: DataSourceSchemaInfo[] | null = normalizeSchema(eagerSchema) ?? normalizeSchema(dsDetail?.schema_info);
  const isSchemaLoading = isLoading && !eagerSchema;
  const filtered = search.trim()
    ? (schemas ?? []).filter(s => s.table_name.toLowerCase().includes(search.toLowerCase()))
    : (schemas ?? []);

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
      onRefresh();
    },
    onError: (err: unknown) => { if (err instanceof ApiError) message.error(err.message); },
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
      onRefresh();
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

  const openEditColumn = (tableName: string, col: { name: string; type: string; description?: string | null; nullable?: boolean }) => {
    setEditColTarget({ table: tableName, column: col.name });
    editColForm.setFieldsValue({ name: col.name, description: col.description, nullable: col.nullable ?? true });
    setEditColType(col.type);
    setEditColOpen(true);
  };

  const dbType = (dataSource as any).db_type ?? 'mysql';
  const colTypeConfig = DB_TYPES[dbType] ?? DB_TYPES.mysql;

  function ColTypeSelect({ placeholder }: { placeholder?: string }) {
    return (
      <Select
        showSearch
        placeholder={placeholder ?? t('datasources.selectType')}
        style={{ width: 160 }}
        options={[
          ...colTypeConfig.types.map(typeOpt => ({ label: typeOpt, value: typeOpt })),
          { label: `— ${t('datasources.customTypeHint')}`, value: '__custom__', disabled: true },
        ]}
      />
    );
  }

  const body = (
    <>
      {isSchemaLoading ? (
        <div style={{ textAlign: 'center', padding: 40 }}><Spin /></div>
      ) : (
        <>
          <Alert
            type="info"
            message={t('datasources.manualSchemaTip')}
            showIcon
            style={{ marginBottom: 16 }}
            action={
              <Button size="small" type="primary" icon={<PlusOutlined />}
                onClick={() => setAddTableOpen(true)}>
                {t('datasources.addTable')}
              </Button>
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
                    <Typography.Text strong style={{ fontSize: 13 }}>{table.table_name}</Typography.Text>
                    {table.is_manual && <Tag color="gold" style={{ fontSize: 10 }}>manual</Tag>}
                    {table.description && (
                      <Typography.Text type="secondary" style={{ fontSize: 11, marginLeft: 4 }}>— {table.description}</Typography.Text>
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
                      {table.columns.length > 0 ? (
                        <Table
                          size="small"
                          pagination={false}
                          dataSource={table.columns}
                          rowKey="name"
                          columns={[
                            { title: '#', key: 'idx', width: 40, render: (_: unknown, __: unknown, i: number) => i + 1 },
                            {
                              title: t('datasources.columnName'),
                              dataIndex: 'name',
                              key: 'name',
                              render: (v: string, record: { name: string; type: string; description?: string | null; nullable?: boolean; is_manual?: boolean }) => (
                                <Space>
                                  <Typography.Text code style={{ fontSize: 12 }}>{v}</Typography.Text>
                                  {record.is_manual && <Tag color="gold" style={{ fontSize: 10 }}>manual</Tag>}
                                </Space>
                              ),
                            },
                            { title: t('datasources.columnType'), dataIndex: 'type', key: 'type', width: 140, render: (v: string) => <Tag>{v}</Tag> },
                            {
                              title: t('datasources.nullable'),
                              dataIndex: 'nullable',
                              key: 'nullable',
                              width: 80,
                              align: 'center' as const,
                              render: (v: boolean) => v ? <Tag color="default">NULL</Tag> : <Tag color="purple">NOT NULL</Tag>,
                            },
                            {
                              title: t('datasources.columnDesc'),
                              dataIndex: 'description',
                              key: 'description',
                              render: (v?: string) => (
                                <Typography.Text style={{ fontSize: 12, color: v ? 'var(--text-primary)' : 'var(--text-muted)' }}>
                                  {v || '—'}
                                </Typography.Text>
                              ),
                            },
                            {
                              title: '',
                              key: 'action',
                              width: 100,
                              render: (_: unknown, record: { name: string; type: string; description?: string | null; nullable?: boolean; is_manual?: boolean }) => (
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
                          <Typography.Text type="secondary" style={{ fontSize: 12 }}>{t('datasources.noColumns')}</Typography.Text>
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
            <Input placeholder="e.g. user_sessions" />
          </Form.Item>
          <Form.Item name="description" label={t('datasources.tableDescription')}>
            <Input.TextArea rows={2} placeholder={t('datasources.tableDescriptionPlaceholder')} />
          </Form.Item>
          <Divider>{t('datasources.tabColumns')}</Divider>
          <Form.List name="columns">
            {(fields, { add, remove }) => (
              <>
                {fields.map(({ key, name }) => (
                  <Space key={key} style={{ display: 'flex', marginBottom: 8, alignItems: 'flex-start' }}>
                    <Form.Item name={[name, 'col_name']} rules={[{ required: true }]} style={{ marginBottom: 0 }}>
                      <Input placeholder="name" style={{ width: 120 }} />
                    </Form.Item>
                    <Form.Item name={[name, 'col_type']} style={{ marginBottom: 0 }}>
                      <ColTypeSelect placeholder={t('datasources.selectType')} />
                    </Form.Item>
                    <Form.Item name={[name, 'col_desc']} style={{ marginBottom: 0 }}>
                      <Input placeholder="description" style={{ width: 120 }} />
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
            <Input.TextArea rows={3} />
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
        <Form form={addColForm} layout="vertical" onFinish={(values) => {
          if (!colType) { message.error(t('datasources.selectType')); return; }
          addColMutation.mutate({
            name: values.name,
            type: colType,
            description: values.description,
            nullable: values.nullable,
          });
        }}>
          <Form.Item name="name" label={t('datasources.columnName')} rules={[{ required: true }]}>
            <Input placeholder="e.g. created_at" />
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
            <Switch checkedChildren="NULL" unCheckedChildren="NOT NULL" />
          </Form.Item>
          <Form.Item name="description" label={t('datasources.columnDesc')}>
            <Input.TextArea rows={2} />
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
        <Form form={editColForm} layout="vertical" onFinish={(values) => {
          if (!editColType) { message.error(t('datasources.selectType')); return; }
          putColMutation.mutate({
            name: values.name,
            type: editColType,
            description: values.description,
            nullable: values.nullable,
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
            <Switch checkedChildren="NULL" unCheckedChildren="NOT NULL" />
          </Form.Item>
          <Form.Item name="description" label={t('datasources.columnDesc')}>
            <Input.TextArea rows={2} />
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

export { SchemaManagementDrawer };
