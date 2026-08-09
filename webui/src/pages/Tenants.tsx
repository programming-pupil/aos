import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Card,
  Table,
  Tag,
  Typography,
  Button,
  Space,
  Modal,
  Form,
  Input,
  Select,
  Popconfirm,
  Tooltip,
  message,
  Progress,
  Alert,
  Row,
  Col,
  Drawer,
  Descriptions,
  Divider,
} from 'antd';
import {
  PlusOutlined,
  EditOutlined,
  DeleteOutlined,
  WarningOutlined,
  UserOutlined,
  GlobalOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { tenantsApi } from '@/api';
import { queryKeys } from '@/api/queryKeys';
import { PageSkeleton } from '@/components/Skeleton';
import type { TenantInfo } from '@/types';

const { Title, Text } = Typography;

const PLAN_COLORS: Record<string, string> = {
  free: 'default',
  starter: 'cyan',
  pro: 'blue',
  enterprise: 'purple',
  trial: 'orange',
};

const PLAN_LABELS: Record<string, { zh: string; en: string }> = {
  free: { zh: '免费版', en: 'Free' },
  starter: { zh: '入门版', en: 'Starter' },
  pro: { zh: '专业版', en: 'Pro' },
  enterprise: { zh: '企业版', en: 'Enterprise' },
  trial: { zh: '试用版', en: 'Trial' },
};

interface TenantUsage {
  tenant_id: string;
  usage_this_month: number;
  max_tokens_monthly?: number | null;
  user_count: number;
  max_users?: number | null;
  usage_percent: number;
  over_limit: boolean;
}

function QuotaBar({ used, max, label }: { used: number; max: number; label: string }) {
  const pct = max > 0 ? Math.min((used / max) * 100, 100) : 0;
  const color = pct > 100 ? '#ff4d4f' : pct > 80 ? '#faad14' : '#52c41a';
  const format = (n: number) =>
    n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1_000 ? `${(n / 1_000).toFixed(0)}K` : String(n);

  return (
    <div style={{ marginBottom: 8 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4 }}>
        <Text type="secondary" style={{ fontSize: 12 }}>{label}</Text>
        <Text style={{ fontSize: 12 }}>
          {format(used)}{max > 0 ? ` / ${format(max)}` : ''}
          {pct > 100 && <WarningOutlined style={{ color: '#ff4d4f', marginLeft: 4 }} />}
        </Text>
      </div>
      {max > 0 && (
        <Progress percent={Math.min(pct, 100)} size="small" showInfo={false} strokeColor={color} />
      )}
    </div>
  );
}

function PlanComparison({ usageData }: { usageData: Record<string, TenantUsage> }) {
  const { t } = useTranslation();
  const plans = [
    { key: 'free', label: t('tenants.plans.free'), users: 5, tokens: 100_000, mcp: 3, skills: 2, price: '$0' },
    { key: 'starter', label: t('tenants.plans.starter'), users: 20, tokens: 1_000_000, mcp: 10, skills: 10, price: '$29' },
    { key: 'pro', label: t('tenants.plans.pro'), users: 100, tokens: 10_000_000, mcp: 50, skills: 50, price: '$99' },
    { key: 'enterprise', label: t('tenants.plans.enterprise'), users: -1, tokens: -1, mcp: -1, skills: -1, price: t('tenants.customPrice') },
  ];
  const col = (v: number) => v < 0 ? <Text type="secondary">{'\\u221E'}</Text> : String(v);
  const tcol = (v: number) => v < 0 ? <Text type="secondary">{'\\u221E'}</Text> : v >= 1_000_000 ? `${(v / 1_000_000).toFixed(0)}M` : `${(v / 1_000).toFixed(0)}K`;

  return (
    <Card title={t('tenants.planComparison')} size="small">
      <Table
        size="small"
        pagination={false}
        columns={[
          { title: '', dataIndex: 'label', key: 'label', render: (v: string) => <Text strong>{v}</Text> },
          { title: t('tenants.planCols.users'), dataIndex: 'users', key: 'users', align: 'center' as const, render: col },
          { title: t('tenants.planCols.tokensMonthly'), dataIndex: 'tokens', key: 'tokens', align: 'center' as const, render: tcol },
          { title: t('tenants.planCols.mcpServers'), dataIndex: 'mcp', key: 'mcp', align: 'center' as const, render: col },
          { title: t('tenants.planCols.skills'), dataIndex: 'skills', key: 'skills', align: 'center' as const, render: col },
          { title: t('tenants.planCols.price'), dataIndex: 'price', key: 'price', align: 'center' as const, render: (v: string) => <Text strong>{v}</Text> },
        ]}
        dataSource={plans.map((p) => ({ ...p, key: p.key }))}
      />
    </Card>
  );
}

export default function Tenants() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [editingTenant, setEditingTenant] = useState<TenantInfo | null>(null);
  const [detailTenant, setDetailTenant] = useState<TenantInfo | null>(null);
  const [usageData, setUsageData] = useState<Record<string, TenantUsage>>({});
  const [form] = Form.useForm();

  const { data, isLoading } = useQuery({
    queryKey: queryKeys.tenants.list(),
    queryFn: () => tenantsApi.list({ per_page: 100 }),
  });

  const tenants: TenantInfo[] = data?.tenants ?? [];

  const fetchUsage = async (tenantId: string) => {
    try {
      const usage = await tenantsApi.getUsage(tenantId);
      setUsageData((prev) => ({ ...prev, [tenantId]: usage }));
    } catch {
      // non-admins may not have access
    }
  };

  const createMutation = useMutation({
    mutationFn: tenantsApi.create,
    onSuccess: () => {
      message.success(t('tenants.addSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.tenants.all });
      setDrawerOpen(false);
      form.resetFields();
    },
    onError: (err: Error) => message.error(err.message || t('common.operateFailed')),
  });

  const updateMutation = useMutation({
    mutationFn: ({ id, data: d }: { id: string; data: Record<string, unknown> }) =>
      tenantsApi.update(id, d as Parameters<typeof tenantsApi.update>[1]),
    onSuccess: () => {
      message.success(t('tenants.editSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.tenants.all });
      setDrawerOpen(false);
      setEditingTenant(null);
      form.resetFields();
    },
    onError: (err: Error) => message.error(err.message || t('common.operateFailed')),
  });

  const deleteMutation = useMutation({
    mutationFn: tenantsApi.delete,
    onSuccess: () => {
      message.success(t('tenants.deleteSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.tenants.all });
    },
    onError: (err: Error) => {
      if (err.message?.includes('default tenant')) {
        message.error(t('tenants.cannotDeleteDefault'));
      } else {
        message.error(err.message || t('common.operateFailed'));
      }
    },
  });

  const columns: ColumnsType<TenantInfo> = [
    {
      title: t('tenants.columns.name'),
      dataIndex: 'name',
      key: 'name',
      render: (name: string, r: TenantInfo) => (
        <Space>
          <div style={{
            width: 36, height: 36, borderRadius: 8,
            background: 'var(--accent-ai-muted, rgba(24,144,255,0.12))',
            display: 'flex', alignItems: 'center', justifyContent: 'center',
            fontSize: 14, color: 'var(--accent-ai, #1677ff)',
            fontWeight: 600,
          }}>
            {name?.[0]?.toUpperCase() ?? r.slug[0].toUpperCase()}
          </div>
          <div>
            <Text strong style={{ fontSize: 14 }}>{name || t('tenants.defaultName')}</Text>
            <br />
            <Text type="secondary" style={{ fontSize: 12 }}>{r.slug}</Text>
          </div>
        </Space>
      ),
    },
    {
      title: t('tenants.columns.plan'),
      dataIndex: 'plan',
      key: 'plan',
      width: 110,
      render: (plan: string) => (
        <Tag color={PLAN_COLORS[plan] ?? 'default'}>
          {PLAN_LABELS[plan]?.zh ?? plan}
        </Tag>
      ),
    },
    {
      title: t('tenants.columns.userCount'),
      dataIndex: 'user_count',
      key: 'user_count',
      width: 110,
      align: 'right' as const,
      render: (count: number | undefined, r: TenantInfo) => {
        const used = count ?? 0;
        const max = r.max_users;
        return (
          <Space size={4}>
            <UserOutlined style={{ fontSize: 12, color: '#999' }} />
            <Text style={{ fontSize: 13 }}>{used}</Text>
            {max && <Text type="secondary" style={{ fontSize: 11 }}>/{max}</Text>}
          </Space>
        );
      },
    },
    {
      title: t('tenants.columns.tokenQuota'),
      key: 'quota',
      width: 200,
      render: (_: unknown, r: TenantInfo) => {
        const usage = usageData[r.id];
        const used = usage?.usage_this_month ?? 0;
        const max = usage?.max_tokens_monthly ?? r.max_tokens_monthly ?? 0;
        const pct = usage?.usage_percent ?? 0;
        const overLimit = usage?.over_limit ?? false;
        const color = overLimit ? '#ff4d4f' : pct > 80 ? '#faad14' : '#52c41a';
        const fmt = (n: number) =>
          n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1_000 ? `${(n / 1_000).toFixed(0)}K` : String(n);

        if (max === 0) {
          return (
            <Space size={4}>
              <GlobalOutlined style={{ fontSize: 12, color: '#999' }} />
              <Text style={{ fontSize: 13 }}>{fmt(used)}</Text>
              <Tag color="default" style={{ fontSize: 10 }}>{t('tenants.noLimit')}</Tag>
            </Space>
          );
        }
        return (
          <Tooltip title={`${used.toLocaleString()} / ${max.toLocaleString()} (${pct.toFixed(1)}%)`}>
            <div style={{ minWidth: 120 }}>
              <Progress percent={Math.min(pct, 100)} size="small" showInfo={false} strokeColor={color} style={{ marginBottom: 2 }} />
              <Text style={{ fontSize: 11 }}>
                {fmt(used)} / {fmt(max)}
                {overLimit && <WarningOutlined style={{ color: '#ff4d4f', marginLeft: 4 }} />}
              </Text>
            </div>
          </Tooltip>
        );
      },
    },
    {
      title: t('tenants.columns.createdAt'),
      dataIndex: 'created_at',
      key: 'created_at',
      width: 120,
      render: (v: string) =>
        v ? <Text type="secondary" style={{ fontSize: 12 }}>{new Date(v).toLocaleDateString()}</Text> : null,
    },
    {
      title: t('tenants.columns.actions'),
      key: 'actions',
      width: 150,
      render: (_: unknown, r: TenantInfo) => (
        <Space size={4}>
          <Tooltip title={t('common.edit')}>
            <Button type="text" size="small" icon={<EditOutlined />} onClick={() => {
              setEditingTenant(r);
              form.setFieldsValue({ name: r.name, slug: r.slug, plan: r.plan, max_users: r.max_users, max_tokens_monthly: r.max_tokens_monthly });
              setDrawerOpen(true);
            }} />
          </Tooltip>
          <Tooltip title={t('tenants.viewDetails')}>
            <Button type="text" size="small" icon={<GlobalOutlined />} onClick={() => {
              setDetailTenant(r);
              if (!usageData[r.id]) fetchUsage(r.id);
            }} />
          </Tooltip>
          {!r.is_system && (
            <Popconfirm
              title={t('tenants.deleteConfirm', { name: r.name })}
              onConfirm={() => deleteMutation.mutate(r.id)}
              okText={t('common.delete')}
              cancelText={t('common.cancel')}
              okButtonProps={{ danger: true }}
            >
              <Tooltip title={t('common.delete')}>
                <Button type="text" size="small" danger icon={<DeleteOutlined />} loading={deleteMutation.isPending} />
              </Tooltip>
            </Popconfirm>
          )}
        </Space>
      ),
    },
  ];

  if (isLoading) return <PageSkeleton rows={5} />;

  return (
    <div style={{ padding: 24, height: '100%', overflow: 'auto' }}>
      <div style={{ marginBottom: 24, display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between' }}>
        <div>
          <Title level={3} style={{ margin: '0 0 4px' }}>{t('tenants.title')}</Title>
          <Text type="secondary">{t('tenants.subtitle')}</Text>
        </div>
        <Button type="primary" icon={<PlusOutlined />} onClick={() => {
          setEditingTenant(null);
          form.resetFields();
          form.setFieldsValue({ plan: 'free' });
          setDrawerOpen(true);
        }}>
          {t('tenants.add')}
        </Button>
      </div>

      <PlanComparison usageData={usageData} />

      <Card styles={{ body: { padding: 0 } }} style={{ marginTop: 16 }}>
        <Table
          rowKey="id"
          columns={columns}
          dataSource={tenants}
          pagination={{ total: data?.total ?? 0, pageSize: 20, showSizeChanger: false }}
          locale={{ emptyText: t('tenants.empty.title') }}
        />
      </Card>

      {/* Create / Edit Drawer */}
      <Drawer
        title={editingTenant ? t('tenants.edit') : t('tenants.add')}
        open={drawerOpen}
        onClose={() => { setDrawerOpen(false); setEditingTenant(null); form.resetFields(); }}
        width={480}
        footer={
          <Space style={{ width: '100%', justifyContent: 'flex-end' }}>
            <Button onClick={() => { setDrawerOpen(false); setEditingTenant(null); }}>
              {t('common.cancel')}
            </Button>
            <Button
              type="primary"
              loading={createMutation.isPending || updateMutation.isPending}
              onClick={async () => {
                try {
                  const values = await form.validateFields();
                  if (editingTenant) {
                    updateMutation.mutate({ id: editingTenant.id, data: values });
                  } else {
                    createMutation.mutate(values as Parameters<typeof tenantsApi.create>[0]);
                  }
                } catch {
                  // validation failed
                }
              }}
            >
              {editingTenant ? t('common.save') : t('common.create')}
            </Button>
          </Space>
        }
      >
        <Form form={form} layout="vertical" requiredMark="optional">
          <Form.Item name="name" label={t('tenants.form.name')} rules={[{ required: true, message: t('common.required') }]}>
            <Input placeholder={t('tenants.form.namePlaceholder')} />
          </Form.Item>
          <Form.Item
            name="slug"
            label={t('tenants.form.slug')}
            rules={[
              { required: true, message: t('common.required') },
              { pattern: /^[a-z0-9-]+$/, message: t('tenants.form.slugPatternError') },
            ]}
            extra={editingTenant ? t('tenants.slugExtra') : undefined}
          >
            <Input placeholder={t('tenants.form.slugPlaceholder')} disabled={!!editingTenant} />
          </Form.Item>
          <Form.Item name="plan" label={t('tenants.form.plan')} rules={[{ required: true }]} initialValue="free">
            <Select>
              <Select.Option value="free">{t('tenants.plans.free')}</Select.Option>
              <Select.Option value="starter">{t('tenants.plans.starter')}</Select.Option>
              <Select.Option value="pro">{t('tenants.plans.pro')}</Select.Option>
              <Select.Option value="enterprise">{t('tenants.plans.enterprise')}</Select.Option>
            </Select>
          </Form.Item>
          <Divider style={{ margin: '12px 0' }} />
          <Form.Item name="max_users" label={t('tenants.form.maxUsers')} extra={t('tenants.form.maxUsersExtra')}>
            <Input type="number" placeholder={t('tenants.form.maxUsersPlaceholder')} min={1} />
          </Form.Item>
          <Form.Item name="max_tokens_monthly" label={t('tenants.form.maxTokensMonthly')}>
            <Input type="number" placeholder={t('tenants.form.maxTokensMonthlyPlaceholder')} min={0} />
          </Form.Item>
        </Form>
      </Drawer>

      {/* Detail Drawer */}
      <Drawer
        title={detailTenant?.name ?? ''}
        open={!!detailTenant}
        onClose={() => setDetailTenant(null)}
        width={560}
      >
        {detailTenant && (
          <>
            <Descriptions column={2} size="small" bordered style={{ marginBottom: 16 }}>
              <Descriptions.Item label={t('tenants.columns.slug')}>
                <code style={{ fontSize: 12 }}>{detailTenant.slug}</code>
              </Descriptions.Item>
              <Descriptions.Item label={t('tenants.columns.plan')}>
                <Tag color={PLAN_COLORS[detailTenant.plan] ?? 'default'}>
                  {PLAN_LABELS[detailTenant.plan]?.zh ?? detailTenant.plan}
                </Tag>
              </Descriptions.Item>
              <Descriptions.Item label={t('tenants.columns.createdAt')}>
                {new Date(detailTenant.created_at).toLocaleString()}
              </Descriptions.Item>
              <Descriptions.Item label={t('tenants.columns.userCount')}>
                <Space><UserOutlined />{detailTenant.user_count ?? 0}</Space>
              </Descriptions.Item>
            </Descriptions>

            <Title level={5} style={{ margin: '0 0 12px' }}>{t('tenants.quotaUsage')}</Title>
            <Card size="small">
              <QuotaBar
                used={usageData[detailTenant.id]?.usage_this_month ?? 0}
                max={usageData[detailTenant.id]?.max_tokens_monthly ?? detailTenant.max_tokens_monthly ?? 0}
                label={t('tenants.columns.tokenQuota')}
              />
              <QuotaBar
                used={usageData[detailTenant.id]?.user_count ?? detailTenant.user_count ?? 0}
                max={usageData[detailTenant.id]?.max_users ?? detailTenant.max_users ?? 0}
                label={t('tenants.columns.userCount')}
              />
              {(usageData[detailTenant.id]?.over_limit ?? false) && (
                <Alert
                  type="error"
                  icon={<WarningOutlined />}
                  message={t('tenants.overLimit')}
                  showIcon
                  style={{ marginTop: 8 }}
                />
              )}
            </Card>
          </>
        )}
      </Drawer>
    </div>
  );
}
