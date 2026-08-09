import { useState, useEffect } from 'react';
import { useNavigate } from '@/router';
import { Form, Input, Button, Card, Typography, message, Divider, Space, Select, Steps, Alert } from 'antd';
import { CheckCircleOutlined, KeyOutlined, LockOutlined, TeamOutlined, UserOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import { apiKeysApi, setupApi } from '@/api';
import { LanguageSwitcher } from '@/components/LanguageSwitcher';
import { useAuthStore } from '@/store/auth';
import { queryClient } from '@/queryClient';

const { Title, Text, Paragraph } = Typography;

type ProviderPreset = 'deepseek' | 'kimi' | 'glm' | 'gemini' | 'xai' | 'openai' | 'anthropic' | 'custom';

interface ApiKeySetupValues {
  provider_preset: ProviderPreset;
  name: string;
  base_url?: string;
  model: string;
  key_value: string;
}

const PROVIDER_PRESETS: Record<ProviderPreset, { name: string; baseUrl?: string; model?: string }> = {
  deepseek: { name: 'DeepSeek', baseUrl: 'https://api.deepseek.com/v1' },
  kimi: { name: 'Kimi', baseUrl: 'https://api.moonshot.cn/v1', model: 'kimi-k2.5' },
  glm: { name: 'GLM', baseUrl: 'https://open.bigmodel.cn/api/paas/v4', model: 'glm-5' },
  gemini: {
    name: 'Gemini',
    baseUrl: 'https://generativelanguage.googleapis.com/v1beta/openai',
    model: 'gemini-2.5-pro',
  },
  xai: { name: 'Grok', baseUrl: 'https://api.x.ai/v1', model: 'grok-3' },
  openai: { name: 'OpenAI' },
  anthropic: { name: 'Anthropic' },
  custom: { name: '' },
};

const OPENAI_COMPATIBLE_PRESET_PROVIDERS = new Set<ProviderPreset>([
  'deepseek',
  'kimi',
  'glm',
  'gemini',
]);
const EDITABLE_BASE_URL_PRESET_PROVIDERS = new Set<ProviderPreset>([
  ...OPENAI_COMPATIBLE_PRESET_PROVIDERS,
  'xai',
]);

export default function Setup() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { login } = useAuthStore();
  const [loading, setLoading] = useState(false);
  const [apiKeyLoading, setApiKeyLoading] = useState(false);
  const [status, setStatus] = useState<'loading' | 'fresh'>('loading');
  const [step, setStep] = useState<'organization' | 'apiKey'>('organization');
  const [apiKeyForm] = Form.useForm<ApiKeySetupValues>();
  const providerPreset = Form.useWatch('provider_preset', apiKeyForm) ?? 'deepseek';

  useEffect(() => {
    let cancelled = false;
    setupApi
      .check()
      .then((res) => {
        if (cancelled) return;
        if (res.initialized) {
          navigate('/login', { replace: true });
          return;
        }
        setStatus('fresh');
      })
      .catch(() => {
        if (cancelled) return;
        setStatus('fresh');
      });
    return () => {
      cancelled = true;
    };
  }, [navigate]);

  const onFinish = async (values: {
    tenant_name: string;
    tenant_slug: string;
    admin_email: string;
    admin_name: string;
    admin_password: string;
    admin_password_confirm: string;
  }) => {
    if (values.admin_password !== values.admin_password_confirm) {
      message.error(t('setup.passwordMismatch'));
      return;
    }
    setLoading(true);
    try {
      const res = await setupApi.init({
        tenant_name: values.tenant_name,
        tenant_slug: values.tenant_slug,
        admin_email: values.admin_email,
        admin_name: values.admin_name,
        admin_password: values.admin_password,
      });
      queryClient.clear();
      login(res.token, {
        id: res.admin_user_id,
        email: values.admin_email,
        name: values.admin_name,
        role: 'admin',
        tenant_id: res.tenant_id,
        menu_permissions_inherited: true,
        menu_permissions: [],
      });
      message.success(t('setup.initSuccess'));
      setStep('apiKey');
    } catch {
      message.error(t('setup.initFailed'));
    } finally {
      setLoading(false);
    }
  };

  const finishSetup = () => {
    queryClient.clear();
    window.dispatchEvent(new Event('aos-setup-complete'));
    navigate('/dashboard');
  };

  const onApiKeyFinish = async (values: ApiKeySetupValues) => {
    const preset = values.provider_preset;
    const provider = OPENAI_COMPATIBLE_PRESET_PROVIDERS.has(preset) || preset === 'custom'
      ? 'custom'
      : preset;
    const baseUrl = values.base_url?.trim();
    const usesCustomBaseUrl = EDITABLE_BASE_URL_PRESET_PROVIDERS.has(preset) || preset === 'custom';
    if (usesCustomBaseUrl && !baseUrl) {
      message.error(t('setup.apiKeyBaseUrlRequired'));
      return;
    }

    setApiKeyLoading(true);
    try {
      await apiKeysApi.create({
        name: values.name.trim(),
        provider,
        base_url: usesCustomBaseUrl ? baseUrl : undefined,
        model: values.model.trim(),
        model_type: 'chat',
        key_value: values.key_value.trim(),
        priority: 100,
        scenarios: ['chat', 'nl2sql', 'rd', 'pm', 'agent'],
        capabilities_json: null,
      });
      message.success(t('setup.apiKeySuccess'));
      finishSetup();
    } catch {
      message.error(t('setup.apiKeyFailed'));
    } finally {
      setApiKeyLoading(false);
    }
  };

  const applyProviderPreset = (preset: ProviderPreset) => {
    const defaults = PROVIDER_PRESETS[preset];
    apiKeyForm.setFieldsValue({
      provider_preset: preset,
      name: defaults.name,
      base_url: defaults.baseUrl,
      model: defaults.model,
    });
  };

  if (status === 'loading') {
    return (
      <div style={{
        minHeight: '100vh',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        position: 'relative',
        background: 'var(--bg-void)',
      }}>
        <div style={{ position: 'absolute', top: 16, right: 16, zIndex: 1 }}>
          <LanguageSwitcher />
        </div>
        <div style={{ textAlign: 'center' }}>
          <div style={{
            width: 48,
            height: 48,
            borderRadius: 12,
            background: 'linear-gradient(135deg, var(--accent-ai) 0%, #a855f7 100%)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            margin: '0 auto 16px',
            fontSize: 20,
            fontWeight: 800,
            color: '#fff',
          }}>
            A
          </div>
          <Text type="secondary">{t('common.loading')}</Text>
        </div>
      </div>
    );
  }

  return (
    <div style={{
      minHeight: '100vh',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      position: 'relative',
      background: 'var(--bg-void)',
      padding: 24,
    }}>
      <div style={{ position: 'absolute', top: 16, right: 16, zIndex: 1 }}>
        <LanguageSwitcher />
      </div>
      <div style={{ maxWidth: 520, width: '100%' }}>
        {/* Header */}
        <div style={{ textAlign: 'center', marginBottom: 32 }}>
          <div style={{
            width: 64,
            height: 64,
            borderRadius: 16,
            background: 'linear-gradient(135deg, var(--accent-ai) 0%, #a855f7 100%)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            margin: '0 auto 16px',
            fontSize: 28,
            fontWeight: 800,
            color: '#fff',
            boxShadow: '0 8px 32px rgba(139, 92, 246, 0.35)',
          }}>
            A
          </div>
          <Title level={2} style={{ margin: '0 0 8px', fontFamily: 'var(--font-code)' }}>
            {t('setup.title')}
          </Title>
          <Text type="secondary">{t('setup.subtitle')}</Text>
          <Steps
            size="small"
            current={step === 'organization' ? 0 : 1}
            items={[{ title: t('setup.step1Short') }, { title: t('setup.step2Short') }]}
            style={{ marginTop: 24 }}
          />
        </div>

        <Card
          style={{
            borderRadius: 8,
            boxShadow: '0 8px 32px rgba(0,0,0,0.08)',
            border: '1px solid var(--border-subtle)',
          }}
          styles={{ body: { padding: 32 } }}
        >
          {step === 'organization' ? (
            <>
              <div style={{ marginBottom: 20 }}>
                <Space align="center" style={{ marginBottom: 4 }}>
                  <TeamOutlined style={{ color: 'var(--accent-ai)', fontSize: 16 }} />
                  <Title level={5} style={{ margin: 0 }}>{t('setup.step1Title')}</Title>
                </Space>
                <Text type="secondary" style={{ fontSize: 13 }}>
                  {t('setup.step1Subtitle')}
                </Text>
              </div>
              <Divider style={{ margin: '16px 0' }} />

              <Form
                layout="vertical"
                onFinish={onFinish}
                size="large"
                requiredMark={false}
              >
            <Form.Item
              label={t('setup.tenantName')}
              name="tenant_name"
              rules={[{ required: true, message: t('setup.tenantNamePlaceholder') }]}
            >
              <Input
                placeholder={t('setup.tenantNamePlaceholder')}
                prefix={<TeamOutlined style={{ color: 'var(--text-muted)' }} />}
              />
            </Form.Item>

            <Form.Item
              label={t('setup.tenantSlug')}
              name="tenant_slug"
              rules={[
                { required: true, message: t('setup.tenantSlugPlaceholder') },
                { pattern: /^[a-z0-9-]+$/, message: t('setup.tenantSlugPlaceholder') },
              ]}
            >
              <Input
                placeholder={t('setup.tenantSlugPlaceholder')}
                prefix={<span style={{ color: 'var(--text-muted)', fontFamily: 'var(--font-code)', fontSize: 12 }}>slug/</span>}
              />
            </Form.Item>

            <Divider style={{ margin: '16px 0' }} />

            <Form.Item
              label={t('setup.adminEmail')}
              name="admin_email"
              rules={[
                { required: true, type: 'email', message: t('setup.adminEmailPlaceholder') },
              ]}
            >
              <Input
                placeholder={t('setup.adminEmailPlaceholder')}
                prefix={<UserOutlined style={{ color: 'var(--text-muted)' }} />}
              />
            </Form.Item>

            <Form.Item
              label={t('setup.adminName')}
              name="admin_name"
              rules={[{ required: true, message: t('setup.adminNamePlaceholder') }]}
            >
              <Input
                placeholder={t('setup.adminNamePlaceholder')}
                prefix={<UserOutlined style={{ color: 'var(--text-muted)' }} />}
              />
            </Form.Item>

            <Form.Item
              label={t('setup.adminPassword')}
              name="admin_password"
              rules={[
                { required: true, message: t('setup.adminPasswordPlaceholder') },
                { min: 8, message: t('setup.adminPasswordPlaceholder') },
              ]}
            >
              <Input.Password
                placeholder={t('setup.adminPasswordPlaceholder')}
                prefix={<LockOutlined style={{ color: 'var(--text-muted)' }} />}
              />
            </Form.Item>

            <Form.Item
              label={t('setup.adminPasswordConfirm')}
              name="admin_password_confirm"
              dependencies={['admin_password']}
              rules={[
                { required: true, message: t('setup.adminPasswordConfirm') },
                ({ getFieldValue }) => ({
                  validator(_, value) {
                    if (!value || getFieldValue('admin_password') === value) {
                      return Promise.resolve();
                    }
                    return Promise.reject(new Error(t('setup.passwordMismatch')));
                  },
                }),
              ]}
            >
              <Input.Password
                placeholder={t('setup.adminPasswordConfirm')}
                prefix={<LockOutlined style={{ color: 'var(--text-muted)' }} />}
              />
            </Form.Item>

            <Form.Item style={{ marginBottom: 0, marginTop: 24 }}>
              <Button
                type="primary"
                htmlType="submit"
                block
                size="large"
                loading={loading}
              >
                {t('setup.initButton')}
              </Button>
            </Form.Item>
              </Form>
            </>
          ) : (
            <>
              <div style={{ marginBottom: 20 }}>
                <Space align="center" style={{ marginBottom: 4 }}>
                  <KeyOutlined style={{ color: 'var(--accent-ai)', fontSize: 16 }} />
                  <Title level={5} style={{ margin: 0 }}>{t('setup.step2Title')}</Title>
                </Space>
                <Paragraph type="secondary" style={{ fontSize: 13, margin: 0 }}>
                  {t('setup.step2Subtitle')}
                </Paragraph>
              </div>
              <Alert
                type="success"
                showIcon
                icon={<CheckCircleOutlined />}
                message={t('setup.defaultsInitialized')}
                description={t('setup.defaultsInitializedHint')}
                style={{ marginBottom: 20 }}
              />
              <Form<ApiKeySetupValues>
                form={apiKeyForm}
                layout="vertical"
                onFinish={onApiKeyFinish}
                size="large"
                requiredMark={false}
                initialValues={{
                  provider_preset: 'deepseek',
                  name: 'DeepSeek',
                  base_url: 'https://api.deepseek.com/v1',
                  model: '',
                }}
              >
                <Form.Item name="provider_preset" label={t('setup.apiKeyProvider')}>
                  <Select
                    onChange={applyProviderPreset}
                    options={[
                      { value: 'deepseek', label: 'DeepSeek' },
                      { value: 'kimi', label: t('apikeys.providerKimi') },
                      { value: 'glm', label: t('apikeys.providerGLM') },
                      { value: 'gemini', label: t('apikeys.providerGemini') },
                      { value: 'xai', label: t('apikeys.providerXAI') },
                      { value: 'openai', label: 'OpenAI' },
                      { value: 'anthropic', label: 'Anthropic' },
                      { value: 'custom', label: t('setup.apiKeyCustomProvider') },
                    ]}
                  />
                </Form.Item>
                <Form.Item
                  name="name"
                  label={t('setup.apiKeyName')}
                  rules={[{ required: true, message: t('common.required') }]}
                >
                  <Input prefix={<KeyOutlined />} placeholder={t('setup.apiKeyNamePlaceholder')} />
                </Form.Item>
                {(EDITABLE_BASE_URL_PRESET_PROVIDERS.has(providerPreset) || providerPreset === 'custom') && (
                  <Form.Item
                    name="base_url"
                    label={t('setup.apiKeyBaseUrl')}
                    rules={[{ required: true, type: 'url', message: t('setup.apiKeyBaseUrlRequired') }]}
                  >
                    <Input placeholder="https://api.example.com/v1" />
                  </Form.Item>
                )}
                <Form.Item
                  name="model"
                  label={t('setup.apiKeyModel')}
                  rules={[{ required: true, message: t('common.required') }]}
                >
                  <Input placeholder={PROVIDER_PRESETS[providerPreset].model ?? (providerPreset === 'deepseek' ? 'deepseek-chat' : t('setup.apiKeyModelPlaceholder'))} />
                </Form.Item>
                <Form.Item
                  name="key_value"
                  label={t('setup.apiKeyValue')}
                  rules={[{ required: true, min: 8, message: t('setup.apiKeyValueRequired') }]}
                >
                  <Input.Password prefix={<LockOutlined />} autoComplete="new-password" />
                </Form.Item>
                <Space direction="vertical" size={8} style={{ width: '100%', marginTop: 8 }}>
                  <Button type="primary" htmlType="submit" block loading={apiKeyLoading}>
                    {t('setup.saveApiKey')}
                  </Button>
                  <Button block onClick={finishSetup} disabled={apiKeyLoading}>
                    {t('setup.skipApiKey')}
                  </Button>
                </Space>
              </Form>
            </>
          )}
        </Card>
      </div>
    </div>
  );
}
