import { useState, useEffect } from 'react';
import { Form, Input, Button, Card, message, Typography } from 'antd';
import { UserOutlined, LockOutlined } from '@ant-design/icons';
import { useNavigate } from '@/router';
import { useTranslation } from 'react-i18next';
import { authApi } from '@/api';
import { LanguageSwitcher } from '@/components/LanguageSwitcher';
import { useAuthStore } from '@/store/auth';
import { queryClient } from '@/queryClient';

const { Title, Text } = Typography;

export default function Login() {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(false);
  const navigate = useNavigate();
  const { login } = useAuthStore();

  const onFinish = async (values: { email: string; password: string }) => {
    setLoading(true);
    try {
      const res = await authApi.login(values);
      queryClient.clear();
      login(res.token, res.user);
      message.success(t('auth.loginSuccess'));
      navigate('/dashboard');
    } catch {
      message.error(t('auth.loginFailed'));
    } finally {
      setLoading(false);
    }
  };

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
      <div style={{ maxWidth: 420, width: '100%' }}>
        {/* Brand */}
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
            fontFamily: 'var(--font-code)',
            boxShadow: '0 8px 32px rgba(139, 92, 246, 0.35)',
          }}>
            A
          </div>
          <Title
            level={2}
            style={{ marginBottom: 4, fontFamily: 'var(--font-code)' }}
          >
            Agent OS
          </Title>
          <Text type="secondary">{t('common.appFullName')}</Text>
        </div>

        <Card
          style={{
            borderRadius: 16,
            boxShadow: '0 8px 32px rgba(0,0,0,0.08)',
            border: '1px solid var(--border-subtle)',
          }}
          styles={{ body: { padding: 32 } }}
        >
          <Form name="login" onFinish={onFinish} size="large" layout="vertical">
            <Form.Item
              label={t('auth.email')}
              name="email"
              rules={[{ required: true, type: 'email' }]}
            >
              <Input prefix={<UserOutlined style={{ color: 'var(--text-muted)' }} />} placeholder={t('auth.email')} />
            </Form.Item>
            <Form.Item
              label={t('auth.password')}
              name="password"
              rules={[{ required: true }]}
            >
              <Input.Password prefix={<LockOutlined style={{ color: 'var(--text-muted)' }} />} placeholder={t('auth.password')} />
            </Form.Item>
            <Form.Item style={{ marginBottom: 16, marginTop: 8 }}>
              <Button type="primary" htmlType="submit" loading={loading} block size="large">
                {t('auth.login')}
              </Button>
            </Form.Item>
          </Form>
        </Card>
      </div>
    </div>
  );
}
