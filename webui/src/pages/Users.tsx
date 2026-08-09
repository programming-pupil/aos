import { useCallback, useMemo, useState } from 'react';
import { useNavigate } from '@/router';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import {
  Card,
  Table,
  Tag,
  Typography,
  Button,
  Modal,
  Form,
  Input,
  Space,
  Popconfirm,
  message,
  Tooltip,
  Switch,
  Avatar,
  Divider,
  Checkbox,
  Alert,
} from 'antd';
import {
  PlusOutlined,
  EditOutlined,
  StopOutlined,
  UndoOutlined,
  CopyOutlined,
  CheckCircleFilled,
  MailOutlined,
  ClockCircleOutlined,
} from '@ant-design/icons';
import type { ColumnsType } from 'antd/es/table';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { usersApi } from '@/api';
import { ApiError } from '@/api/errors';
import { queryKeys } from '@/api/queryKeys';
import { PageSkeleton } from '@/components/Skeleton';
import { useAuthStore } from '@/store/auth';
import {
  MENU_CONFIGURABLE_PERMISSIONS,
  getRolePermissions,
  normalizeMenuPermissionsForUi,
  usePermissions,
} from '@/store/permissions';
import type { UserInfo } from '@/types';
import type { Permission } from '@/store/permissions';

const { Title, Text } = Typography;

const MENU_PERMISSION_LABEL_KEYS: Partial<Record<Permission, string>> = {
  'dashboard:read': 'nav.dashboard',
  'super_assistant:read': 'nav.superAssistant',
  'workspace:read': 'nav.workspace',
  'chat:read': 'nav.chat',
  'adversarial:read': 'nav.adversarial',
  'watchdog:read': 'nav.watchdog',
  'tasks:read': 'nav.tasks',
  'rd_studio:read': 'nav.agent',
  'rd_specs:read': 'nav.rdSpecs',
  'rd_quality:read': 'nav.rdQuality',
  'rd_agents:read': 'nav.rdAgents',
  'operations_assistant:read': 'nav.operationsAssistant',
  'operations_tasks:read': 'nav.operationsTasks',
  'operations_materials:read': 'nav.operationsMaterials',
  'operations_governance:read': 'nav.operationsGovernance',
  'operations_governance:write': 'users.permissions.operationsGovernanceBudgetSwitch',
  'projects:read': 'nav.projects',
  'pipeline:read': 'nav.pipeline',
  'nl2sql_explore:read': 'nav.nl2sql',
  'nl2sql_management:read': 'nav.sqlKnowledge',
  'nl2sql_analytics:read': 'nav.nl2sqlAnalytics',
  'datasources:read': 'nav.datasources',
  'mcp:read': 'nav.mcp',
  'skills:read': 'nav.skills',
  'search_providers:read': 'nav.searchProviders',
  'hooks:read': 'nav.hooks',
  'bot_agents:read': 'nav.botAgents',
  'apikeys:read': 'nav.apikeys',
  'users:read': 'nav.users',
  'config:read': 'nav.configManagement',
};

function getDefaultMenuPermissions(role: string): Permission[] {
  return getRolePermissions(role).filter((permission) =>
    MENU_CONFIGURABLE_PERMISSIONS.includes(permission)
  );
}

function formatRelativeTime(
  dateStr: string | null | undefined,
  t: TFunction,
): React.ReactNode {
  if (!dateStr) return <Text type="secondary" style={{ fontSize: 12 }}>—</Text>;
  const date = new Date(dateStr);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMins / 60);
  const diffDays = Math.floor(diffHours / 24);

  let label: string;
  if (diffMins < 1) label = t('users.relative.justNow');
  else if (diffMins < 60) label = t('users.relative.minutesAgo', { count: diffMins });
  else if (diffHours < 24) label = t('users.relative.hoursAgo', { count: diffHours });
  else if (diffDays < 7) label = t('users.relative.daysAgo', { count: diffDays });
  else label = date.toLocaleDateString();

  return (
    <Tooltip title={date.toLocaleString()}>
      <Space size={4}>
        <ClockCircleOutlined style={{ fontSize: 11, color: '#999' }} />
        <Text type="secondary" style={{ fontSize: 12 }}>{label}</Text>
      </Space>
    </Tooltip>
  );
}

export default function Users() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { user: currentUser, token, login } = useAuthStore();
  const { hasPermission } = usePermissions();
  const [form] = Form.useForm();
  const [editForm] = Form.useForm();
  const [inviteModal, setInviteModal] = useState(false);
  const [editUser, setEditUser] = useState<UserInfo | null>(null);
  const [inviteResult, setInviteResult] = useState<{
    invite_url: string;
    email_configured: boolean;
    email_sent: boolean;
    email_error?: string | null;
  } | null>(null);
  const [resetEmailLoadingIds, setResetEmailLoadingIds] = useState<Set<string>>(() => new Set());
  const defaultInviteMenuPermissions = useMemo(() => getDefaultMenuPermissions('developer'), []);
  const menuPermissionOptions = useMemo(
    () =>
      MENU_CONFIGURABLE_PERMISSIONS.map((permission) => ({
        label: MENU_PERMISSION_LABEL_KEYS[permission]
          ? t(MENU_PERMISSION_LABEL_KEYS[permission])
          : permission,
        value: permission,
      })),
    [t]
  );

  const handleMenuPermissionsChange = useCallback(
    (next: Array<Permission | string>) => {
      const normalized = new Set(next as Permission[]);
      if (normalized.has('operations_governance:write')) {
        normalized.add('operations_governance:read');
      }
      return Array.from(normalized);
    },
    [],
  );

  const { data, isLoading } = useQuery({
    queryKey: queryKeys.users.list(),
    queryFn: () => usersApi.list(),
    enabled: hasPermission('users:read'),
  });

  const inviteMutation = useMutation({
    mutationFn: (values: { email: string; name: string; menu_permissions?: string[] }) =>
      usersApi.invite({
        email: values.email,
        name: values.name,
        role: 'developer',
        invite: true,
        menu_permissions: values.menu_permissions ?? [],
      }),
    onSuccess: (res) => {
      if (res.email_sent) {
        message.success(t('users.emailStatus.inviteSent'));
      } else if (!res.email_configured) {
        message.warning(t('users.emailStatus.smtpMissing'));
      } else {
        message.error(t('users.emailStatus.sendFailed'));
      }
      setInviteResult({
        invite_url: res.invite_url,
        email_configured: res.email_configured,
        email_sent: res.email_sent,
        email_error: res.email_error,
      });
      qc.invalidateQueries({ queryKey: queryKeys.users.list() });
    },
    onError: (error) => {
      if (error instanceof ApiError && error.status === 409) {
        message.error(t('users.emailAlreadyRegistered'));
        return;
      }
      message.error(t('common.operateFailed'));
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({ id, data }: { id: string; data: Parameters<typeof usersApi.update>[1] }) =>
      usersApi.update(id, data),
    onSuccess: (updatedUser) => {
      message.success(t('users.updateSuccess'));
      if (token && updatedUser.id === currentUser?.id) {
        login(token, updatedUser);
      }
      setEditUser(null);
      qc.invalidateQueries({ queryKey: queryKeys.users.list() });
    },
    onError: () => {
      message.error(t('common.operateFailed'));
    },
  });

  const deactivateMutation = useMutation({
    mutationFn: (id: string) => usersApi.delete(id),
    onSuccess: () => {
      message.success(t('users.deactivateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.users.list() });
    },
    onError: () => {
      message.error(t('common.operateFailed'));
    },
  });

  const reactivateMutation = useMutation({
    mutationFn: (id: string) => usersApi.update(id, { is_active: true }),
    onSuccess: () => {
      message.success(t('users.reactivateSuccess'));
      qc.invalidateQueries({ queryKey: queryKeys.users.list() });
    },
    onError: () => {
      message.error(t('common.operateFailed'));
    },
  });

  const resetEmailMutation = useMutation({
    mutationFn: (id: string) => usersApi.sendResetEmail(id),
    onMutate: (id) => {
      setResetEmailLoadingIds((prev) => {
        const next = new Set(prev);
        next.add(id);
        return next;
      });
    },
    onSuccess: (res) => {
      if (res.email_sent) {
        message.success(t('users.resetEmailSent'));
      } else if (!res.email_configured) {
        message.warning(t('users.emailStatus.smtpMissing'));
      } else {
        message.error(t('users.emailStatus.sendFailed'));
      }
    },
    onError: () => {
      message.error(t('common.operateFailed'));
    },
    onSettled: (_data, _error, id) => {
      setResetEmailLoadingIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    },
  });

  const handleInvite = (values: { email: string; name: string; menu_permissions?: string[] }) => {
    inviteMutation.mutate(values);
  };

  const handleEdit = (values: {
    name?: string;
    is_active?: boolean;
    menu_permissions?: string[];
  }) => {
    if (!editUser) return;
    updateMutation.mutate({
      id: editUser.id,
      data: {
        name: values.name,
        is_active: values.is_active,
        menu_permissions_inherited: false,
        menu_permissions: values.menu_permissions ?? [],
      },
    });
  };

  const copyInviteLink = () => {
    if (inviteResult?.invite_url) {
      navigator.clipboard.writeText(inviteResult.invite_url).then(() => {
        message.success(t('users.inviteLinkCopied'));
      });
    }
  };

  const columns: ColumnsType<UserInfo> = [
    {
      title: t('users.columns.name'),
      dataIndex: 'name',
      key: 'name',
      render: (name: string, record) => (
        <Space>
          <Avatar
            style={{
              background: 'var(--accent-ai-muted)',
              color: 'var(--accent-ai)',
              fontWeight: 600,
              flexShrink: 0,
            }}
            size={36}
          >
            {name?.[0]?.toUpperCase() ?? record.email[0].toUpperCase()}
          </Avatar>
          <div>
            <Text strong style={{ fontSize: 14 }}>{name || record.email.split('@')[0]}</Text>
            {record.is_active === false && (
              <Tag color="red" style={{ marginLeft: 6, fontSize: 10 }}>{t('users.status.inactive')}</Tag>
            )}
          </div>
        </Space>
      ),
    },
    {
      title: t('users.columns.email'),
      dataIndex: 'email',
      key: 'email',
      render: (email: string) => <Text type="secondary" style={{ fontSize: 13 }}>{email}</Text>,
    },
    {
      title: t('users.columns.menuPermissions'),
      key: 'menu_permissions',
      width: 180,
      render: (_, record) => {
        if (record.menu_permissions_inherited ?? true) {
          const count = getDefaultMenuPermissions(record.role).length;
          return <Tag color="blue">{t('users.menuPermissions.defaultCount', { count })}</Tag>;
        }
        const count = normalizeMenuPermissionsForUi(record.menu_permissions).length;
        return (
          <Tag color={count > 0 ? 'green' : 'default'}>
            {t('users.menuPermissions.customCount', { count })}
          </Tag>
        );
      },
    },
    {
      title: t('users.columns.lastLogin'),
      dataIndex: 'last_login_at',
      key: 'last_login_at',
      width: 140,
      render: (v: string | null) => formatRelativeTime(v, t),
    },
    {
      title: t('users.columns.createdAt'),
      dataIndex: 'created_at',
      key: 'created_at',
      width: 120,
      render: (created_at: string) =>
        created_at ? (
          <Text type="secondary" style={{ fontSize: 12 }}>
            {new Date(created_at).toLocaleDateString()}
          </Text>
        ) : null,
    },
    {
      title: t('users.columns.actions'),
      key: 'actions',
      width: 160,
      render: (_, record) => (
        <Space size={4}>
          <Tooltip title={t('common.edit')}>
            <Button
              type="text"
              size="small"
              icon={<EditOutlined />}
              onClick={() => {
                setEditUser(record);
                const defaultMenuPermissions = getDefaultMenuPermissions(record.role);
                const inherited = record.menu_permissions_inherited ?? true;
                editForm.setFieldsValue({
                  name: record.name,
                  is_active: record.is_active !== false,
                  menu_permissions: inherited
                    ? defaultMenuPermissions
                    : normalizeMenuPermissionsForUi(record.menu_permissions),
                });
              }}
              disabled={!hasPermission('users:write')}
            />
          </Tooltip>
          <Tooltip title={t('users.form.sendResetEmail')}>
            <Button
              type="text"
              size="small"
              icon={<MailOutlined />}
              onClick={() => resetEmailMutation.mutate(record.id)}
              loading={resetEmailLoadingIds.has(record.id)}
              disabled={!hasPermission('users:write')}
            />
          </Tooltip>
          {record.is_active === false ? (
            <Popconfirm
              title={t('users.reactivateConfirm')}
              onConfirm={() => reactivateMutation.mutate(record.id)}
              disabled={!hasPermission('users:write')}
            >
              <Tooltip title={t('users.reactivate')}>
                <Button
                  type="text"
                  size="small"
                  icon={<UndoOutlined />}
                  loading={reactivateMutation.isPending && reactivateMutation.variables === record.id}
                  disabled={!hasPermission('users:write')}
                />
              </Tooltip>
            </Popconfirm>
          ) : (
            <Popconfirm
              title={t('users.deactivateConfirm')}
              onConfirm={() => deactivateMutation.mutate(record.id)}
              disabled={!hasPermission('users:delete')}
            >
              <Tooltip title={t('users.deactivate')}>
                <Button
                  type="text"
                  size="small"
                  icon={<StopOutlined />}
                  loading={deactivateMutation.isPending && deactivateMutation.variables === record.id}
                  disabled={!hasPermission('users:delete')}
                />
              </Tooltip>
            </Popconfirm>
          )}
        </Space>
      ),
    },
  ];

  if (isLoading) return <PageSkeleton />;

  return (
    <div style={{ padding: 24, height: '100%', overflow: 'auto' }}>
      <div style={{ marginBottom: 24, display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between' }}>
        <div>
          <Title level={3} style={{ margin: '0 0 4px' }}>{t('users.title')}</Title>
          <Text type="secondary">{t('users.subtitle')}</Text>
        </div>
        {hasPermission('users:write') && (
          <Button
            type="primary"
            icon={<PlusOutlined />}
            onClick={() => {
              form.resetFields();
              setInviteResult(null);
              setInviteModal(true);
            }}
          >
            {t('users.add')}
          </Button>
        )}
      </div>

      <Card styles={{ body: { padding: 0 } }}>
        <Table
          rowKey="id"
          columns={columns}
          dataSource={data?.users ?? []}
          pagination={{
            total: data?.total ?? 0,
            pageSize: 20,
            showSizeChanger: false,
            showTotal: (total) => `${total} ${t('users.columns.total')}`,
          }}
          locale={{ emptyText: t('users.empty.title') }}
        />
      </Card>

      {/* Invite / Create User Modal */}
      <Modal
        title={editUser ? t('users.form.editTitle') : t('users.form.inviteTitle')}
        open={inviteModal || !!editUser}
        onCancel={() => {
          setInviteModal(false);
          setEditUser(null);
          setInviteResult(null);
          form.resetFields();
          editForm.resetFields();
        }}
        footer={null}
        destroyOnHidden
        width={editUser ? 520 : 480}
      >
        {inviteResult ? (
          <div style={{ textAlign: 'center', padding: '16px 0' }}>
            <div style={{
              width: 56,
              height: 56,
              borderRadius: '50%',
              background: '#f6ffed',
              border: '1px solid #b7eb8f',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              margin: '0 auto 12px',
              fontSize: 24,
            }}>
              <CheckCircleFilled style={{ color: '#52c41a' }} />
            </div>
            <Title level={5}>{t('users.inviteModal.title')}</Title>
            <Text type="secondary" style={{ display: 'block', marginBottom: 12 }}>
              {t('users.inviteModal.description')}
            </Text>
            {inviteResult.email_sent ? (
              <Alert
                type="success"
                showIcon
                style={{ textAlign: 'left', marginBottom: 12 }}
                message={t('users.emailStatus.inviteSent')}
              />
            ) : !inviteResult.email_configured ? (
              <Alert
                type="warning"
                showIcon
                style={{ textAlign: 'left', marginBottom: 12 }}
                message={t('users.emailStatus.smtpMissing')}
                description={
                  <Space direction="vertical" size={8}>
                    <Text type="secondary">{t('users.emailStatus.smtpMissingHint')}</Text>
                    <Button
                      size="small"
                      onClick={() => {
                        setInviteModal(false);
                        navigate('/config/management');
                      }}
                    >
                      {t('users.emailStatus.goConfig')}
                    </Button>
                  </Space>
                }
              />
            ) : (
              <Alert
                type="error"
                showIcon
                style={{ textAlign: 'left', marginBottom: 12 }}
                message={t('users.emailStatus.sendFailed')}
                description={inviteResult.email_error ?? t('users.emailStatus.checkConfig')}
              />
            )}
            {!inviteResult.email_sent && (
              <Alert
                type="info"
                showIcon
                style={{ textAlign: 'left', marginBottom: 12 }}
                message={t('users.emailStatus.manualDeliveryTitle')}
                description={t('users.emailStatus.manualDeliveryHint')}
              />
            )}
            <Input.Group compact style={{ display: 'flex' }}>
              <Input value={inviteResult.invite_url} readOnly style={{ flex: 1, fontFamily: 'monospace', fontSize: 12 }} />
              <Button icon={<CopyOutlined />} onClick={copyInviteLink} />
            </Input.Group>
          </div>
        ) : editUser ? (
          /* Edit User Form */
          <Form
            form={editForm}
            layout="vertical"
            onFinish={handleEdit}
            initialValues={{
              name: editUser.name,
              is_active: editUser.is_active !== false,
              menu_permissions: editUser.menu_permissions_inherited
                ? getDefaultMenuPermissions(editUser.role)
                : normalizeMenuPermissionsForUi(editUser.menu_permissions),
            }}
          >
            {/* Email (read-only) */}
            <Form.Item label={t('users.columns.email')} style={{ marginBottom: 8 }}>
              <Input value={editUser.email} disabled />
            </Form.Item>

            <Form.Item label={t('users.columns.name')} name="name" rules={[{ required: true }]}>
              <Input placeholder={t('users.form.namePlaceholder')} />
            </Form.Item>

            <Divider style={{ margin: '12px 0' }} />

            <Form.Item
              label={t('users.form.isActive')}
              name="is_active"
              valuePropName="checked"
              style={{ marginBottom: 8 }}
            >
              <Switch checkedChildren={t('users.status.active')} unCheckedChildren={t('users.status.inactive')} />
            </Form.Item>

            <Form.Item
              label={t('users.form.menuPermissions')}
              name="menu_permissions"
              extra={t('users.form.menuPermissionsHelp')}
              rules={[
                {
                  validator: (_, value: string[] | undefined) =>
                    value && value.length > 0
                      ? Promise.resolve()
                      : Promise.reject(new Error(t('users.form.menuPermissionsRequired'))),
                },
              ]}
            >
              <Checkbox.Group
                options={menuPermissionOptions}
                onChange={(checked) => {
                  editForm.setFieldValue('menu_permissions', handleMenuPermissionsChange(checked));
                }}
                style={{
                  display: 'grid',
                  gridTemplateColumns: 'repeat(2, minmax(0, 1fr))',
                  gap: 8,
                }}
              />
            </Form.Item>

            {/* Password Reset */}
            <div style={{ marginBottom: 16 }}>
              <Button
                type="link"
                icon={<MailOutlined />}
                onClick={() => resetEmailMutation.mutate(editUser.id)}
                loading={resetEmailLoadingIds.has(editUser.id)}
                style={{ padding: 0 }}
                disabled={!hasPermission('users:write')}
              >
                {t('users.form.sendResetEmail')}
              </Button>
              {editUser.password_changed_at && (
                <Text type="secondary" style={{ fontSize: 11, marginLeft: 8 }}>
                  {t('users.columns.passwordChanged')}: {new Date(editUser.password_changed_at).toLocaleDateString()}
                </Text>
              )}
            </div>

            <Divider style={{ margin: '12px 0' }} />

            {/* Last Login */}
            {editUser.last_login_at && (
              <div style={{ marginBottom: 16 }}>
                <Space>
                  <ClockCircleOutlined style={{ color: '#999', fontSize: 12 }} />
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {t('users.columns.lastLogin')}: {new Date(editUser.last_login_at).toLocaleString()}
                  </Text>
                </Space>
              </div>
            )}

            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 24 }}>
              <Button onClick={() => { setEditUser(null); editForm.resetFields(); }}>
                {t('common.cancel')}
              </Button>
              <Button type="primary" htmlType="submit" loading={updateMutation.isPending}>
                {t('common.save')}
              </Button>
            </div>
          </Form>
        ) : (
          /* Invite / Create User Form */
          <Form
            form={form}
            layout="vertical"
            onFinish={handleInvite}
            initialValues={{ menu_permissions: defaultInviteMenuPermissions }}
          >
            <Alert
              type="info"
              showIcon
              style={{ marginBottom: 16 }}
              message={t('users.inviteFlow.title')}
              description={
                <Space direction="vertical" size={4}>
                  <Text>{t('users.inviteFlow.description')}</Text>
                  <Text type="secondary" style={{ fontSize: 12 }}>
                    {t('users.inviteFlow.smtpHint')}
                  </Text>
                </Space>
              }
            />

            <Form.Item
              label={t('users.form.email')}
              name="email"
              rules={[
                { required: true, message: t('common.required') },
                { type: 'email', message: t('users.form.emailInvalid') },
              ]}
            >
              <Input placeholder={t('users.form.emailPlaceholder')} />
            </Form.Item>

            <Form.Item label={t('users.form.name')} name="name" rules={[{ required: true, message: t('common.required') }]}>
              <Input placeholder={t('users.form.namePlaceholder')} />
            </Form.Item>

            <Form.Item
              label={t('users.form.menuPermissions')}
              name="menu_permissions"
              extra={t('users.form.menuPermissionsHelp')}
              rules={[
                {
                  validator: (_, value: string[] | undefined) =>
                    value && value.length > 0
                      ? Promise.resolve()
                      : Promise.reject(new Error(t('users.form.menuPermissionsRequired'))),
                },
              ]}
            >
              <Checkbox.Group
                options={menuPermissionOptions}
                onChange={(checked) => {
                  form.setFieldValue('menu_permissions', handleMenuPermissionsChange(checked));
                }}
                style={{
                  display: 'grid',
                  gridTemplateColumns: 'repeat(2, minmax(0, 1fr))',
                  gap: 8,
                }}
              />
            </Form.Item>

            <Divider style={{ margin: '12px 0' }} />

            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 24 }}>
              <Button onClick={() => { setInviteModal(false); form.resetFields(); }}>
                {t('common.cancel')}
              </Button>
              <Button type="primary" htmlType="submit" loading={inviteMutation.isPending}>
                {t('users.form.sendInvite')}
              </Button>
            </div>
          </Form>
        )}
      </Modal>
    </div>
  );
}
