import { useState } from 'react';
import { useSearchParams, useNavigate } from '@/router';
import {
  Card,
  Form,
  Input,
  Button,
  Typography,
  Divider,
  message,
  Space,
  Alert,
} from 'antd';
import { CheckCircleOutlined, LinkOutlined, LockOutlined, SafetyCertificateOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import { useMutation } from '@tanstack/react-query';
import { authApi } from '@/api';
import { LanguageSwitcher } from '@/components/LanguageSwitcher';

const { Title, Text } = Typography;

export default function InvitePage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const [form] = Form.useForm();
  const [isSuccess, setIsSuccess] = useState(false);

  const inviteToken = searchParams.get('token');

  const acceptInvite = useMutation({
    mutationFn: (password: string) =>
      authApi.acceptInvite({ password, invite_token: inviteToken ?? '' }),
    onSuccess: () => {
      setIsSuccess(true);
      message.success(t('invite.success'));
      setTimeout(() => navigate('/login'), 2000);
    },
    onError: (err: Error) => {
      message.error(err.message ?? t('invite.failed'));
    },
  });

  const handleSubmit = (values: { password: string; confirmPassword: string }) => {
    if (values.password !== values.confirmPassword) {
      message.error(t('invite.passwordMismatch'));
      return;
    }
    acceptInvite.mutate(values.password);
  };

  if (!inviteToken) {
    return (
      <div
        style={{
          minHeight: '100vh',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          position: 'relative',
          background: 'var(--bg-void)',
          padding: 24,
        }}
      >
        <div style={{ position: 'absolute', top: 16, right: 16 }}><LanguageSwitcher /></div>
        <Card
          style={{ width: '100%', maxWidth: 420, textAlign: 'center', borderRadius: 8, border: '1px solid var(--border-subtle)' }}
          styles={{ body: { padding: 32 } }}
        >
          <Space direction="vertical" size="large" style={{ width: '100%' }}>
            <div>
              <LinkOutlined style={{ fontSize: 44, color: 'var(--color-warning)' }} />
            </div>
            <div>
              <Title level={4}>{t('invite.invalidTitle')}</Title>
              <Text type="secondary">{t('invite.failed')}</Text>
            </div>
            <Button type="primary" onClick={() => navigate('/login')} block>
              {t('invite.backToLogin')}
            </Button>
          </Space>
        </Card>
      </div>
    );
  }

  if (isSuccess) {
    return (
      <div
        style={{
          minHeight: '100vh',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          position: 'relative',
          background: 'var(--bg-void)',
          padding: 24,
        }}
      >
        <div style={{ position: 'absolute', top: 16, right: 16 }}><LanguageSwitcher /></div>
        <Card
          style={{ width: '100%', maxWidth: 420, textAlign: 'center', borderRadius: 8, border: '1px solid var(--border-subtle)' }}
          styles={{ body: { padding: 32 } }}
        >
          <Space direction="vertical" size="large" style={{ width: '100%' }}>
            <div>
              <CheckCircleOutlined style={{ fontSize: 44, color: 'var(--color-success)' }} />
            </div>
            <div>
              <Title level={4}>{t('invite.success')}</Title>
              <Text type="secondary">{t('invite.successHint')}</Text>
            </div>
            <Button type="primary" onClick={() => navigate('/login')} block>
              {t('login.title')}
            </Button>
          </Space>
        </Card>
      </div>
    );
  }

  return (
    <div
      style={{
        minHeight: '100vh',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        position: 'relative',
        background: 'var(--bg-void)',
        padding: 24,
      }}
    >
      <div style={{ position: 'absolute', top: 16, right: 16 }}><LanguageSwitcher /></div>
      <Card
        style={{ width: '100%', maxWidth: 420, borderRadius: 8, border: '1px solid var(--border-subtle)' }}
        styles={{ body: { padding: 32 } }}
      >
        <Space direction="vertical" size="large" style={{ width: '100%' }}>
          <div style={{ textAlign: 'center' }}>
            <SafetyCertificateOutlined style={{ fontSize: 40, color: 'var(--color-success)' }} />
            <Title level={4} style={{ marginTop: 8, marginBottom: 4 }}>
              {t('invite.title')}
            </Title>
            <Text type="secondary">{t('invite.subtitle')}</Text>
          </div>

          <Alert
            type="info"
            message={t('invite.activationHint')}
            style={{ fontSize: 13 }}
          />

          <Form
            form={form}
            layout="vertical"
            onFinish={handleSubmit}
            requiredMark="optional"
            size="large"
          >
            <Form.Item
              name="password"
              label={t('invite.passwordLabel')}
              rules={[
                { required: true, message: t('common.required') },
                { min: 8, message: t('invite.passwordMinLength') },
              ]}
            >
              <Input.Password
                prefix={<LockOutlined style={{ color: '#aaa' }} />}
                placeholder={t('invite.passwordPlaceholder')}
              />
            </Form.Item>

            <Form.Item
              name="confirmPassword"
              label={t('invite.passwordConfirm')}
              dependencies={['password']}
              rules={[
                { required: true, message: t('common.required') },
                ({ getFieldValue }) => ({
                  validator(_, value) {
                    if (!value || getFieldValue('password') === value) {
                      return Promise.resolve();
                    }
                    return Promise.reject(new Error(t('invite.passwordMismatch')));
                  },
                }),
              ]}
            >
              <Input.Password
                prefix={<LockOutlined style={{ color: '#aaa' }} />}
                placeholder={t('invite.passwordPlaceholder')}
              />
            </Form.Item>

            <Form.Item style={{ marginBottom: 0, marginTop: 8 }}>
              <Button
                type="primary"
                htmlType="submit"
                block
                loading={acceptInvite.isPending}
              >
                {t('invite.acceptInvite')}
              </Button>
            </Form.Item>
          </Form>

          <Divider style={{ margin: '8px 0' }} />

          <Text type="secondary" style={{ fontSize: 12, textAlign: 'center', display: 'block' }}>
            <a onClick={() => navigate('/login')}>{t('invite.backToLogin')}</a>
          </Text>
        </Space>
      </Card>
    </div>
  );
}
