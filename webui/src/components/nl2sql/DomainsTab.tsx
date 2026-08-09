// ── Business Domains Tab — NL2SQL Management Page ────────────────────────────────

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Table, Button, Modal, Form, Input, Select, Space, Tag, message,
  Popconfirm, Typography, Card, Spin, Empty, Tooltip, Badge,
} from 'antd';
import {
  AppstoreOutlined, SyncOutlined, EditOutlined, DeleteOutlined,
  PlusOutlined, DatabaseOutlined, UnorderedListOutlined, QuestionCircleOutlined,
} from '@ant-design/icons';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { nl2sqlApi, dataSourcesApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import type { BusinessDomain, UpdateDomainRequest } from '@/types';

const { Text, Paragraph } = Typography;

interface EditFormValues {
  domainName: string;
  domainDescription?: string;
  domainRoutingMode?: 'assist' | 'strict';
}

interface CreateFormValues {
  domainName: string;
  domainDescription?: string;
  domainRoutingMode?: 'assist' | 'strict';
}

export function DomainsTab() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [form] = Form.useForm<EditFormValues>();
  const [createForm] = Form.useForm<CreateFormValues>();

  const [selectedDsId, setSelectedDsId] = useState<string | undefined>();
  const [editingDomain, setEditingDomain] = useState<BusinessDomain | null>(null);
  const [assignDomain, setAssignDomain] = useState<BusinessDomain | null>(null);
  const [assignSelection, setAssignSelection] = useState<string[]>([]);
  const [createOpen, setCreateOpen] = useState(false);
  const [domainSearch, setDomainSearch] = useState('');
  const [routingModeHelpOpen, setRoutingModeHelpOpen] = useState(false);

  const routingModeLabel = (
    <Space size={4}>
      {t('management.domains.routingMode')}
      <Button
        type="text"
        size="small"
        icon={<QuestionCircleOutlined />}
        aria-label={t('management.domains.routingModeHelpTitle')}
        onClick={() => setRoutingModeHelpOpen(true)}
      />
    </Space>
  );

  // ── Data sources list ────────────────────────────────────────────────────────
  const { data: dsList, isLoading: dsLoading } = useQuery({
    queryKey: queryKeys.dataSources.list(),
    queryFn: () => dataSourcesApi.list({ per_page: 200 }),
  });

  useEffect(() => {
    const firstDatasourceId = dsList?.data_sources?.[0]?.id;
    if (!selectedDsId && firstDatasourceId) {
      setSelectedDsId(firstDatasourceId);
    }
  }, [dsList?.data_sources, selectedDsId]);

  const datasourceId = selectedDsId;

  // ── Domains list ─────────────────────────────────────────────────────────────
  const {
    data: domainsData,
    isLoading: domainsLoading,
  } = useQuery({
    queryKey: queryKeys.nl2sql.domains.list(datasourceId ?? ''),
    queryFn: () => nl2sqlApi.listDomainsForDatasource(datasourceId!),
    enabled: !!datasourceId,
  });

  // ── Table mapping ─────────────────────────────────────────────────────────────
  const {
    data: tableMappingData,
  } = useQuery({
    queryKey: [...queryKeys.nl2sql.domains.list(datasourceId), 'mappings'],
    queryFn: () => nl2sqlApi.listDomainTableMappings(datasourceId!, assignDomain!.id.toString()),
    enabled: !!datasourceId && !!assignDomain,
  });

  // ── Mutations ─────────────────────────────────────────────────────────────────
  const rediscover = useMutation({
    mutationFn: (id: string) => nl2sqlApi.rediscoverDomains(id),
    onSuccess: (res) => {
      message.success(t('management.domains.rediscoverDone', { count: res.domainsDiscovered }));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.domains.list(datasourceId) });
    },
    onError: () => message.error(t('management.domains.failed')),
  });

  const updateDomain = useMutation({
    mutationFn: ({ domainId, data }: { domainId: string; data: UpdateDomainRequest }) =>
      nl2sqlApi.updateDomain(datasourceId!, domainId, data),
    onSuccess: () => {
      message.success(t('management.domains.updated'));
      setEditingDomain(null);
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.domains.list(datasourceId) });
    },
    onError: () => message.error(t('management.domains.failed')),
  });

  const deleteDomain = useMutation({
    mutationFn: (domainId: string) => nl2sqlApi.deleteDomain(datasourceId!, domainId),
    onSuccess: () => {
      message.success(t('management.domains.deleted'));
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.domains.list(datasourceId) });
    },
    onError: (err) => message.error(`${t('management.domains.failed')}: ${(err as Error).message}`),
  });

  const createDomain = useMutation({
    mutationFn: (data: { name: string; description?: string; domainRoutingMode?: 'assist' | 'strict' }) =>
      nl2sqlApi.createBusinessDomain(datasourceId!, {
        name: data.name,
        description: data.description,
        domainRoutingMode: data.domainRoutingMode,
        tableNames: [],
      }),
    onSuccess: () => {
      message.success(t('management.domains.created'));
      setCreateOpen(false);
      createForm.resetFields();
      qc.invalidateQueries({ queryKey: queryKeys.nl2sql.domains.list(datasourceId) });
    },
    onError: (err) => message.error(`${t('management.domains.failed')}: ${(err as Error).message}`),
  });

  const assignTables = useMutation({
    mutationFn: async ({ domainId, tableNames }: { domainId: string; tableNames: string[] }) => {
      if (!datasourceId) throw new Error('datasourceId is empty');
      const rawMappings = (tableMappingData as { mappings?: Array<Record<string, unknown>> } | undefined)?.mappings ?? [];
      const existing = rawMappings
        .map((m) => String(m.tableName ?? m.table_name ?? ''))
        .filter(Boolean);
      const existingSet = new Set(existing);
      const selectedSet = new Set(tableNames);
      const toAdd = tableNames.filter((name) => !existingSet.has(name));
      const toRemove = existing.filter((name) => !selectedSet.has(name));

      if (toAdd.length > 0) {
        await nl2sqlApi.assignTablesToDomain(datasourceId, domainId, toAdd);
      }
      if (toRemove.length > 0) {
        await nl2sqlApi.unassignTablesFromDomain(datasourceId, domainId, toRemove);
      }
    },
    onSuccess: async () => {
      message.success(t('management.domains.updated'));
      setAssignDomain(null);
      setAssignSelection([]);
      await qc.invalidateQueries({ queryKey: queryKeys.nl2sql.domains.list(datasourceId) });
      await qc.invalidateQueries({ queryKey: [...queryKeys.nl2sql.domains.list(datasourceId), 'mappings'] });
    },
    onError: (err) => message.error(`${t('management.domains.failed')}: ${(err as Error).message}`),
  });

  // ── Table columns ─────────────────────────────────────────────────────────────
  const columns = [
    {
      title: t('management.domains.domainName'),
      dataIndex: 'domainName',
      key: 'domainName',
      width: 180,
      render: (name: string, record: BusinessDomain) => (
        <Text strong>{name}</Text>
      ),
    },
    {
      title: t('management.domains.domainDescription'),
      dataIndex: 'domainDescription',
      key: 'domainDescription',
      ellipsis: true,
      render: (desc: string) => desc ? (
        <Paragraph ellipsis={{ rows: 2 }} style={{ margin: 0, fontSize: 12 }}>
          {desc}
        </Paragraph>
      ) : (
        <Text type="secondary" style={{ fontSize: 12 }}>{t('management.domains.descriptionPlaceholder')}</Text>
      ),
    },
    {
      title: t('management.domains.source'),
      dataIndex: 'source',
      key: 'source',
      width: 100,
      render: (source: string) => (
        <Tag color={source === 'auto' ? 'blue' : 'green'}>
          {source === 'auto' ? t('management.domains.autoDiscovered') : t('management.domains.manuallyEdited')}
        </Tag>
      ),
    },
    {
      title: routingModeLabel,
      dataIndex: 'domainRoutingMode',
      key: 'domainRoutingMode',
      width: 110,
      render: (mode: 'assist' | 'strict') => (
        <Tag color={mode === 'strict' ? 'volcano' : 'blue'}>
          {mode === 'strict'
            ? t('management.domains.routingModeStrict')
            : t('management.domains.routingModeAssist')}
        </Tag>
      ),
    },
    {
      title: t('management.domains.confidence'),
      dataIndex: 'confidenceScore',
      key: 'confidenceScore',
      width: 90,
      render: (score: number) => (
        <Text type={score >= 0.7 ? 'success' : score >= 0.5 ? 'warning' : 'secondary'}>
          {(score * 100).toFixed(0)}%
        </Text>
      ),
    },
    {
      title: t('management.domains.tableCount'),
      dataIndex: 'tableCount',
      key: 'tableCount',
      width: 80,
      render: (n: number) => <Badge count={n} showZero color="#1890ff" />,
    },
    {
      title: t('management.domains.tables'),
      dataIndex: 'tables',
      key: 'tables',
      width: 320,
      render: (tables: string[]) => (
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 2, minWidth: 0, overflow: 'hidden' }}>
          {tables.slice(0, 4).map((tbl) => (
            <Tag
              key={tbl}
              title={tbl}
              style={{
                margin: 0,
                maxWidth: '100%',
                overflow: 'hidden',
                textOverflow: 'ellipsis',
                whiteSpace: 'nowrap',
              }}
              icon={<DatabaseOutlined />}
            >
              {tbl}
            </Tag>
          ))}
          {tables.length > 4 && (
            <Tag style={{ marginBottom: 2 }}>+{tables.length - 4}</Tag>
          )}
        </div>
      ),
    },
    {
      title: t('common.actions'),
      key: 'actions',
      width: 150,
      fixed: 'right' as const,
      render: (_: unknown, record: BusinessDomain) => (
        <Space size="small">
          <Tooltip title={t('management.domains.assignTables')}>
            <Button
              size="small"
              icon={<UnorderedListOutlined />}
              onClick={() => setAssignDomain(record)}
            />
          </Tooltip>
          <Tooltip title={t('management.domains.editDomain')}>
            <Button
              size="small"
              icon={<EditOutlined />}
              onClick={() => {
                setEditingDomain(record);
                form.setFieldsValue({
                  domainName: record.domainName,
                  domainDescription: record.domainDescription,
                  domainRoutingMode: record.domainRoutingMode ?? 'assist',
                });
              }}
            />
          </Tooltip>
          <Popconfirm
            title={t('management.domains.deleteConfirm')}
            onConfirm={() => deleteDomain.mutate(record.id.toString())}
          >
            <Tooltip title={t('management.domains.delete')}>
              <Button size="small" danger icon={<DeleteOutlined />} />
            </Tooltip>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  const normalizedDomains = useMemo(() => {
    const raw = (domainsData as { domains?: Array<Record<string, unknown>> } | undefined)?.domains ?? [];
    return raw.map((d) => {
      const id = Number(d.id ?? 0);
      const tablesRaw = d.tables;
      const tables = Array.isArray(tablesRaw) ? tablesRaw.map((x) => String(x)) : [];
      return {
        id,
        datasourceId: String(d.datasourceId ?? d.datasource_id ?? ''),
        domainName: String(d.domainName ?? d.domain_name ?? ''),
        domainDescription: String(d.domainDescription ?? d.domain_description ?? ''),
        tableCount: Number(d.tableCount ?? d.table_count ?? tables.length ?? 0),
        confidenceScore: Number(d.confidenceScore ?? d.confidence_score ?? 0),
        source: ((d.source === 'manual' || d.source === 'auto') ? d.source : 'auto') as 'auto' | 'manual',
        domainRoutingMode: ((d.domainRoutingMode ?? d.domain_routing_mode) === 'strict' ? 'strict' : 'assist') as 'assist' | 'strict',
        tables,
      } satisfies BusinessDomain;
    });
  }, [domainsData]);

  const filteredDomains = normalizedDomains.filter(
    (d) => !domainSearch || d.domainName.toLowerCase().includes(domainSearch.toLowerCase())
  );

  const mappedTableNames = useMemo(() => {
    const rawMappings = (tableMappingData as { mappings?: Array<Record<string, unknown>> } | undefined)?.mappings ?? [];
    return rawMappings
      .map((m) => String(m.tableName ?? m.table_name ?? ''))
      .filter(Boolean);
  }, [tableMappingData]);

  const allSchemaTables = useMemo(() => {
    const ds = dsList?.data_sources?.find((item) => item.id === datasourceId);
    const tables = ds?.schema_info?.tables ?? [];
    return tables.map((tbl) => tbl.table_name).filter(Boolean);
  }, [datasourceId, dsList?.data_sources]);

  useEffect(() => {
    if (!assignDomain) return;
    setAssignSelection(mappedTableNames);
  }, [assignDomain, mappedTableNames]);

  return (
    <div>
      {/* Header controls */}
      <div style={{ display: 'flex', gap: 12, marginBottom: 12, alignItems: 'center', flexWrap: 'wrap' }}>
        <Text style={{ fontSize: 12 }}>{t('management.domains.datasource')}：</Text>
        <Select
          style={{ width: 220 }}
          placeholder={t('management.domains.selectDatasource')}
          allowClear
          value={selectedDsId}
          onChange={(v) => setSelectedDsId(v)}
          options={dsList?.data_sources?.map((ds) => ({ label: ds.name, value: ds.id }))}
          loading={dsLoading}
        />
        <Button
          icon={<SyncOutlined spin={rediscover.isPending} />}
          loading={rediscover.isPending}
          onClick={() => datasourceId && rediscover.mutate(datasourceId)}
          disabled={!datasourceId}
        >
          {t('management.domains.rediscover')}
        </Button>
        <Button
          type="primary"
          icon={<PlusOutlined />}
          onClick={() => setCreateOpen(true)}
          disabled={!datasourceId}
        >
          {t('management.domains.create')}
        </Button>
        <Text type="secondary" style={{ marginLeft: 'auto', fontSize: 12 }}>
          {t('management.domains.domainsDiscovered', { count: filteredDomains.length })}
        </Text>
      </div>

      {/* Domains table */}
      <Table
        dataSource={filteredDomains}
        columns={columns}
        rowKey={(r) => `${r.datasourceId}-${r.id}`}
        loading={domainsLoading}
        pagination={{ pageSize: 20, showSizeChanger: true, showTotal: (total) => t('common.total', { count: total }) }}
        tableLayout="fixed"
        scroll={{ x: 1250 }}
        size="small"
        locale={{
          emptyText: (
            <Empty
              description={datasourceId ? t('management.domains.noDomains') : t('management.domains.selectDatasource')}
              image={Empty.PRESENTED_IMAGE_SIMPLE}
            />
          ),
        }}
      />

      {/* Edit Domain Modal */}
      <Modal
        title={t('management.domains.editDomain')}
        open={!!editingDomain}
        onCancel={() => {
          setEditingDomain(null);
          form.resetFields();
        }}
        footer={null}
        destroyOnHidden
      >
        <Form<EditFormValues>
          form={form}
          layout="vertical"
          onFinish={(values) => {
            if (!editingDomain) return;
            updateDomain.mutate({ domainId: editingDomain.id.toString(), data: values });
          }}
        >
          <Form.Item
            name="domainName"
            label={t('management.domains.domainName')}
            rules={[{ required: true, message: t('common.required') }]}
          >
            <Input placeholder={t('management.domains.domainName')} />
          </Form.Item>
          <Form.Item name="domainDescription" label={t('management.domains.domainDescription')}>
            <Input.TextArea
              rows={3}
              placeholder={t('management.domains.descriptionPlaceholder')}
            />
          </Form.Item>
          <Form.Item name="domainRoutingMode" label={routingModeLabel}>
            <Select
              options={[
                { label: t('management.domains.routingModeAssist'), value: 'assist' },
                { label: t('management.domains.routingModeStrict'), value: 'strict' },
              ]}
            />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Space>
              <Button
                type="primary"
                htmlType="submit"
                loading={updateDomain.isPending}
              >
                {t('management.domains.save')}
              </Button>
              <Button onClick={() => setEditingDomain(null)}>
                {t('management.domains.cancel')}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={t('management.domains.create')}
        open={createOpen}
        onCancel={() => {
          setCreateOpen(false);
          createForm.resetFields();
        }}
        footer={null}
        destroyOnHidden
      >
        <Form<CreateFormValues>
          form={createForm}
          initialValues={{ domainRoutingMode: 'assist' }}
          layout="vertical"
          onFinish={(values) => {
            createDomain.mutate({
              name: values.domainName.trim(),
              description: values.domainDescription?.trim(),
              domainRoutingMode: values.domainRoutingMode ?? 'assist',
            });
          }}
        >
          <Form.Item
            name="domainName"
            label={t('management.domains.domainName')}
            rules={[
              { required: true, message: t('common.required') },
              { min: 2, message: t('management.domains.domainNameMin') },
            ]}
          >
            <Input placeholder={t('management.domains.domainName')} />
          </Form.Item>
          <Form.Item name="domainDescription" label={t('management.domains.domainDescription')}>
            <Input.TextArea rows={3} placeholder={t('management.domains.descriptionPlaceholder')} />
          </Form.Item>
          <Form.Item name="domainRoutingMode" label={routingModeLabel}>
            <Select
              options={[
                { label: t('management.domains.routingModeAssist'), value: 'assist' },
                { label: t('management.domains.routingModeStrict'), value: 'strict' },
              ]}
            />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Space>
              <Button type="primary" htmlType="submit" loading={createDomain.isPending}>
                {t('common.create')}
              </Button>
              <Button onClick={() => {
                setCreateOpen(false);
                createForm.resetFields();
              }}>
                {t('common.cancel')}
              </Button>
            </Space>
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        title={t('management.domains.assignTables')}
        open={!!assignDomain}
        onCancel={() => {
          setAssignDomain(null);
          setAssignSelection([]);
        }}
        onOk={() => {
          if (!assignDomain) return;
          assignTables.mutate({
            domainId: assignDomain.id.toString(),
            tableNames: assignSelection,
          });
        }}
        confirmLoading={assignTables.isPending}
        okButtonProps={{ disabled: !assignDomain }}
        destroyOnHidden
      >
        <div style={{ marginBottom: 12 }}>
          <Text style={{ fontSize: 12, color: 'var(--text-secondary)' }}>
            {assignDomain ? `${t('management.domains.domainName')}：${assignDomain.domainName}` : ''}
          </Text>
        </div>
        <Select
          mode="multiple"
          style={{ width: '100%' }}
          placeholder={t('management.domains.selectTables')}
          value={assignSelection}
          onChange={(vals) => setAssignSelection(vals)}
          options={allSchemaTables.map((tableName) => ({ label: tableName, value: tableName }))}
          loading={!datasourceId}
          allowClear
          showSearch
          optionFilterProp="label"
        />
        <div style={{ marginTop: 10 }}>
          <Text style={{ fontSize: 12, color: 'var(--text-muted)' }}>
            {allSchemaTables.length === 0
              ? t('management.domains.noTablesInDatasource')
              : `${t('management.domains.selectedTablesCount')}：${assignSelection.length}`}
          </Text>
        </div>
      </Modal>

      <Modal
        title={t('management.domains.routingModeHelpTitle')}
        open={routingModeHelpOpen}
        footer={null}
        onCancel={() => setRoutingModeHelpOpen(false)}
      >
        <Space direction="vertical" size={16} style={{ width: '100%' }}>
          <div>
            <Text strong>{t('management.domains.routingModeAssist')}</Text>
            <Paragraph type="secondary" style={{ marginBottom: 0 }}>
              {t('management.domains.routingModeAssistDescription')}
            </Paragraph>
          </div>
          <div>
            <Text strong>{t('management.domains.routingModeStrict')}</Text>
            <Paragraph type="secondary" style={{ marginBottom: 0 }}>
              {t('management.domains.routingModeStrictDescription')}
            </Paragraph>
          </div>
          <div>
            <Text strong>{t('management.domains.routingModeTestTitle')}</Text>
            <Paragraph type="secondary" style={{ marginBottom: 0 }}>
              {t('management.domains.routingModeTestDescription')}
            </Paragraph>
          </div>
        </Space>
      </Modal>
    </div>
  );
}
