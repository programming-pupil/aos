import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Form, Input, Table, Select, Tag, Button, Space, Modal, message, Popconfirm,
  Typography, Empty, Upload, Card, Pagination, AutoComplete, Alert, Tooltip,
} from 'antd';
import {
  PlusOutlined, DeleteOutlined, EditOutlined,
  AppstoreOutlined, UploadOutlined, GlobalOutlined, DownloadOutlined, QuestionCircleOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { exportToCsv, importCsvFile } from '@/utils/csvUtils';
import type {
  SynonymItem, CreateSynonymRequest, UpdateSynonymRequest,
  PaginatedSynonymsResponse,
} from '@/types';

const { Text } = Typography;
const TERM_TYPES = [
  { labelKey: 'management.synonyms.termTypes.alias', value: 'alias' },
  { labelKey: 'management.synonyms.termTypes.domainTerm', value: 'domain_term' },
  { labelKey: 'management.synonyms.termTypes.abbreviation', value: 'abbreviation' },
  { labelKey: 'management.synonyms.termTypes.foreignKeyAlias', value: 'foreign_key_alias' },
];

interface SynonymFormValues {
  term: string;
  canonicalTable: string;
  canonicalColumn: string;
  termType?: string;
}

function termTypeLabel(t: (key: string) => string, value: string): string {
  const map: Record<string, string> = {
    alias: t('management.synonyms.termTypes.alias'),
    domain_term: t('management.synonyms.termTypes.domainTerm'),
    abbreviation: t('management.synonyms.termTypes.abbreviation'),
    foreign_key_alias: t('management.synonyms.termTypes.foreignKeyAlias'),
  };
  return map[value] ?? value;
}

export function SynonymsTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();

  const [selectedDs, setSelectedDs] = useState<string | undefined>(undefined);
  const [createOpen, setCreateOpen] = useState(false);
  const [editingSynonym, setEditingSynonym] = useState<SynonymItem | null>(null);
  const [form] = Form.useForm();
  const [synonymSearch, setSynonymSearch] = useState('');
  const [currentPage, setCurrentPage] = useState(1);
  const [pageSize, setPageSize] = useState(20);
  const [termTypeHelpOpen, setTermTypeHelpOpen] = useState(false);

  const { data: dsList, isError: datasourceLoadFailed } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list({ per_page: 200 }),
  });

  const datasourceId = selectedDs;
  useEffect(() => {
    const first = dsList?.data_sources?.[0]?.id;
    if (!selectedDs && first) setSelectedDs(first);
  }, [dsList?.data_sources, selectedDs]);

  const selectedDatasource = dsList?.data_sources?.find((item) => item.id === datasourceId);
  const tableOptions = useMemo(() => {
    const tables = selectedDatasource?.schema_info?.tables;
    return Array.isArray(tables)
      ? tables
        .map((table) => table.table_name || table.name)
        .filter((name): name is string => !!name)
        .map((name) => ({
          value: name,
          label: <Tooltip title={name}><span>{name}</span></Tooltip>,
        }))
      : [];
  }, [selectedDatasource]);

  const { data, isLoading } = useQuery<PaginatedSynonymsResponse>({
    queryKey: queryKeys.nl2sql.synonyms(datasourceId ?? '', currentPage, pageSize),
    queryFn: () => nl2sqlApi.listSynonyms(datasourceId!, currentPage, pageSize),
    enabled: !!datasourceId,
  });

  const create = useMutation({
    mutationFn: (payload: CreateSynonymRequest) =>
      nl2sqlApi.createSynonym(datasourceId!, payload),
    onSuccess: () => {
      message.success(t('management.synonyms.createSuccess'));
      setCreateOpen(false);
      form.resetFields();
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.synonyms(datasourceId!, currentPage, pageSize) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const update = useMutation({
    mutationFn: ({ id, data: vals }: { id: number; data: UpdateSynonymRequest }) =>
      nl2sqlApi.updateSynonym(datasourceId!, id, vals),
    onSuccess: () => {
      message.success(t('management.synonyms.updateSuccess'));
      setEditingSynonym(null);
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.synonyms(datasourceId!, currentPage, pageSize) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const deleteMut = useMutation({
    mutationFn: (id: number) => nl2sqlApi.deleteSynonym(datasourceId!, id),
    onSuccess: () => {
      message.success(t('management.synonyms.deleteSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.synonyms(datasourceId!, currentPage, pageSize) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const bulkImport = useMutation({
    mutationFn: (synonyms: CreateSynonymRequest[]) =>
      nl2sqlApi.bulkCreateSynonyms(datasourceId!, { synonyms }),
    onSuccess: (result: unknown) => {
      const res = result as { created?: number; skipped?: number };
      message.success(
        t('management.synonyms.importSuccess', {
          created: res.created ?? 0,
          skipped: res.skipped ?? 0,
        }),
      );
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.synonyms(datasourceId!, currentPage, pageSize) });
    },
    onError: (e: Error) => message.error(e?.message ?? t('common.failed')),
  });

  const columns = [
    {
      title: t('management.synonyms.term'),
      dataIndex: 'term',
      key: 'term',
      width: 160,
      render: (v: string) => <Text strong>{v}</Text>,
    },
    {
      title: t('management.synonyms.termType'),
      dataIndex: 'termType',
      key: 'termType',
      width: 130,
      render: (v: string) => <Tag color="blue">{termTypeLabel(t, v)}</Tag>,
    },
    {
      title: t('management.synonyms.tableName'),
      dataIndex: 'canonicalTable',
      key: 'canonicalTable',
      width: 260,
      render: (v: string) => (
        <Tooltip title={v}>
          <Tag
            icon={<AppstoreOutlined />}
            style={{ maxWidth: 240, overflow: 'hidden', textOverflow: 'ellipsis' }}
          >
            {v}
          </Tag>
        </Tooltip>
      ),
    },
    {
      title: t('management.synonyms.columnName'),
      dataIndex: 'canonicalColumn',
      key: 'canonicalColumn',
      width: 140,
      render: (v: string) => <Text code style={{ fontSize: 12 }}>{v}</Text>,
    },
    {
      title: t('management.synonyms.createdBy'),
      dataIndex: 'createdBy',
      key: 'createdBy',
      width: 100,
      render: (v: string | null) => (
        <Text type="secondary">{v ?? '-'}</Text>
      ),
    },
    {
      title: t('management.synonyms.createdAt'),
      dataIndex: 'createdAt',
      key: 'createdAt',
      width: 150,
      render: (v: string) => (
        <Text type="secondary" style={{ fontSize: 12 }}>{v}</Text>
      ),
    },
    {
      title: '',
      key: 'actions',
      width: 140,
      render: (_: unknown, record: SynonymItem) => (
        <Space size="small">
          <Button
            size="small"
            icon={<EditOutlined />}
            onClick={() => {
              setEditingSynonym(record);
              form.setFieldsValue({
                term: record.term,
                canonicalTable: record.canonicalTable,
                canonicalColumn: record.canonicalColumn,
                termType: record.termType,
              });
            }}
          />
          <Popconfirm
            title={t('management.synonyms.deleteConfirm')}
            onConfirm={() => deleteMut.mutate(record.id)}
          >
            <Button size="small" danger icon={<DeleteOutlined />} />
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const synonyms = data?.data ?? [];
  const filteredSynonyms = synonyms.filter((s) => {
    if (!synonymSearch) return true;
    const q = synonymSearch.toLowerCase();
    return (
      s.term.toLowerCase().includes(q) ||
      s.canonicalTable?.toLowerCase().includes(q) ||
      s.canonicalColumn?.toLowerCase().includes(q)
    );
  });

  const termTypeOptions = TERM_TYPES.map((tt) => ({
    label: t(tt.labelKey),
    value: tt.value,
  }));

  const handleExportCsv = () => {
    const rows = filteredSynonyms.map((s) => ({
      term: s.term,
      termType: s.termType,
      canonicalTable: s.canonicalTable,
      canonicalColumn: s.canonicalColumn,
      createdBy: s.createdBy ?? '',
      createdAt: s.createdAt,
    }));
    exportToCsv(rows, `synonyms-${datasourceId}.csv`);
  };

  const handleImportCsv = (file: File) => {
    importCsvFile<Record<'term' | 'termType' | 'canonicalTable' | 'canonicalColumn', string>>(
      file,
      ['term', 'termType', 'canonicalTable', 'canonicalColumn'],
    )
      .then((rows) => {
        if (rows.length === 0) {
          message.warning(t('management.synonyms.noRowsToImport'));
          return;
        }
        bulkImport.mutate(
          rows.map((r) => ({
            term: r.term,
            termType: r.termType || 'alias',
            canonicalTable: r.canonicalTable,
            canonicalColumn: r.canonicalColumn,
          })),
        );
      })
      .catch(() => message.error(t('common.failed')));
    return false;
  };

  const handleDownloadTemplate = () => {
    const headers = ['term', 'termType', 'canonicalTable', 'canonicalColumn'];
    const csv = `${headers.join(',')}\n`;
    const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'synonyms-template.csv';
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    message.success(t('management.synonyms.templateDownloaded'));
  };

  return (
    <div>
      <Card
        size="small"
        style={{ marginBottom: 12, background: 'var(--bg-secondary)', border: '1px solid var(--border-color)' }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
          <GlobalOutlined style={{ color: 'var(--accent-color)' }} />
          <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
            {t('management.synonyms.hint')}
          </Text>
        </div>
        <div style={{ display: 'flex', gap: 12, alignItems: 'center', flexWrap: 'wrap' }}>
          <Text style={{ fontSize: 13 }}>{t('management.synonyms.datasource')}:</Text>
          <Select
            style={{ width: 220 }}
            placeholder={t('management.domains.selectDatasource')}
            allowClear
            value={selectedDs}
            onChange={(v) => setSelectedDs(v)}
            options={dsList?.data_sources?.map((ds) => ({ label: ds.name, value: ds.id }))}
          />
          <Input.Search
            placeholder={t('management.synonyms.searchPlaceholder')}
            style={{ width: 200 }}
            onSearch={setSynonymSearch}
            allowClear
          />
          <Text type="secondary" style={{ marginLeft: 'auto', fontSize: 12 }}>
            {t('management.synonyms.synonymCount', { count: filteredSynonyms.length })}
          </Text>
        </div>
      </Card>

      <div style={{ display: 'flex', gap: 8, marginBottom: 12 }}>
        <Button
          type="primary"
          icon={<PlusOutlined />}
          onClick={() => setCreateOpen(true)}
          disabled={!datasourceId}
        >
          {t('management.synonyms.newSynonym')}
        </Button>
        <Button icon={<UploadOutlined />} onClick={handleExportCsv} disabled={!data?.data?.length}>
          {t('management.synonyms.exportCsv')}
        </Button>
        <Button
          icon={<DownloadOutlined />}
          onClick={handleDownloadTemplate}
        >
          {t('management.synonyms.downloadTemplate')}
        </Button>
        <Upload
          accept=".csv"
          showUploadList={false}
          beforeUpload={handleImportCsv}
          maxCount={1}
        >
          <Button icon={<UploadOutlined />} disabled={!datasourceId}>
            {t('management.synonyms.importCsv')}
          </Button>
        </Upload>
      </div>

      <Table
        dataSource={filteredSynonyms}
        columns={columns}
        rowKey="id"
        loading={isLoading}
        pagination={false}
        size="small"
        scroll={{ x: 900 }}
        locale={{
          emptyText: (
            <Empty
              description={datasourceId ? t('management.synonyms.noSynonyms') : t('management.domains.selectDatasource')}
            />
          ),
        }}
      />
      <div style={{ display: 'flex', justifyContent: 'flex-end', marginTop: 8 }}>
        <Pagination
          current={currentPage}
          pageSize={pageSize}
          total={data?.total ?? 0}
          showSizeChanger
          showTotal={(total) => t('common.total', { count: total })}
          onChange={(page, size) => {
            setCurrentPage(page);
            setPageSize(size);
          }}
          onShowSizeChange={(_, size) => {
            setCurrentPage(1);
            setPageSize(size);
          }}
        />
      </div>

      {/* Create Modal */}
      <Modal
        title={t('management.synonyms.newSynonym')}
        open={createOpen}
        onCancel={() => { setCreateOpen(false); form.resetFields(); }}
        footer={null}
        width={760}
      >
        <Form
          form={form}
          layout="vertical"
          onFinish={(values) => create.mutate(values as SynonymFormValues)}
        >
          <Form.Item
            name="term"
            label={t('management.synonyms.term')}
            rules={[{ required: true, message: t('management.synonyms.enterTerm') }]}
          >
            <Input placeholder={t('management.synonyms.termPlaceholder')} />
          </Form.Item>
          <Form.Item
            name="termType"
            label={(
              <Space size={4}>
                {t('management.synonyms.termType')}
                <Button
                  type="text"
                  size="small"
                  icon={<QuestionCircleOutlined />}
                  aria-label={t('management.synonyms.termTypeHelpTitle')}
                  onClick={() => setTermTypeHelpOpen(true)}
                />
              </Space>
            )}
          >
            <Select options={termTypeOptions} placeholder={t('management.synonyms.selectType')} />
          </Form.Item>
          {(datasourceLoadFailed || tableOptions.length === 0) && (
            <Alert
              type="info"
              showIcon
              style={{ marginBottom: 16 }}
              message={t('management.synonyms.tableLoadFallback')}
            />
          )}
          <Space align="start" style={{ width: '100%' }}>
            <Form.Item
              name="canonicalTable"
              label={t('management.synonyms.tableName')}
              rules={[{ required: true, message: t('management.synonyms.enterTableName') }]}
              style={{ flex: 2, minWidth: 0 }}
            >
              <AutoComplete
                options={tableOptions}
                popupMatchSelectWidth={520}
                placeholder={t('management.synonyms.tableNamePlaceholder')}
                filterOption={(input, option) =>
                  String(option?.value ?? '').toLowerCase().includes(input.toLowerCase())
                }
              />
            </Form.Item>
            <Form.Item
              name="canonicalColumn"
              label={t('management.synonyms.columnName')}
              rules={[{ required: true, message: t('management.synonyms.enterColumnName') }]}
              style={{ flex: 1, minWidth: 220 }}
            >
              <Input placeholder={t('management.synonyms.columnNamePlaceholder')} />
            </Form.Item>
          </Space>
          <Form.Item style={{ marginBottom: 0 }}>
            <Space>
              <Button type="primary" htmlType="submit" loading={create.isPending}>
                {t('management.synonyms.save')}
              </Button>
              <Button onClick={() => { setCreateOpen(false); form.resetFields(); }}>
                {t('management.synonyms.cancel')}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>

      {/* Edit Modal */}
      <Modal
        title={t('management.synonyms.editSynonym')}
        open={!!editingSynonym}
        onCancel={() => setEditingSynonym(null)}
        footer={null}
        width={760}
      >
        <Form
          form={form}
          layout="vertical"
          onFinish={(values) => {
            if (!editingSynonym) return;
            update.mutate({ id: editingSynonym.id, data: values as UpdateSynonymRequest });
          }}
        >
          <Form.Item
            name="term"
            label={t('management.synonyms.term')}
            rules={[{ required: true, message: t('management.synonyms.enterTerm') }]}
          >
            <Input placeholder={t('management.synonyms.termPlaceholder')} />
          </Form.Item>
          <Form.Item
            name="termType"
            label={(
              <Space size={4}>
                {t('management.synonyms.termType')}
                <Button
                  type="text"
                  size="small"
                  icon={<QuestionCircleOutlined />}
                  aria-label={t('management.synonyms.termTypeHelpTitle')}
                  onClick={() => setTermTypeHelpOpen(true)}
                />
              </Space>
            )}
          >
            <Select options={termTypeOptions} />
          </Form.Item>
          <Space align="start" style={{ width: '100%' }}>
            <Form.Item
              name="canonicalTable"
              label={t('management.synonyms.tableName')}
              rules={[{ required: true, message: t('management.synonyms.enterTableName') }]}
              style={{ flex: 2, minWidth: 0 }}
            >
              <AutoComplete
                options={tableOptions}
                popupMatchSelectWidth={520}
                placeholder={t('management.synonyms.tableNamePlaceholder')}
                filterOption={(input, option) =>
                  String(option?.value ?? '').toLowerCase().includes(input.toLowerCase())
                }
              />
            </Form.Item>
            <Form.Item
              name="canonicalColumn"
              label={t('management.synonyms.columnName')}
              rules={[{ required: true, message: t('management.synonyms.enterColumnName') }]}
              style={{ flex: 1, minWidth: 220 }}
            >
              <Input placeholder={t('management.synonyms.columnNamePlaceholder')} />
            </Form.Item>
          </Space>
          <Form.Item style={{ marginBottom: 0 }}>
            <Space>
              <Button type="primary" htmlType="submit" loading={update.isPending}>
                {t('management.synonyms.save')}
              </Button>
              <Button onClick={() => setEditingSynonym(null)}>
                {t('management.synonyms.cancel')}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        open={termTypeHelpOpen}
        title={t('management.synonyms.termTypeHelpTitle')}
        footer={null}
        onCancel={() => setTermTypeHelpOpen(false)}
      >
        <Space direction="vertical" size={12} style={{ width: '100%' }}>
          {TERM_TYPES.map((type) => (
            <div key={type.value}>
              <Text strong>{t(type.labelKey)}</Text>
              <div><Text type="secondary">{t(`management.synonyms.termTypeDescriptions.${type.value}`)}</Text></div>
            </div>
          ))}
        </Space>
      </Modal>
    </div>
  );
}
