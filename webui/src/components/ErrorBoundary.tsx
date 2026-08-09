import { Component, ReactNode, ErrorInfo } from 'react';
import { Button, Space } from 'antd';
import { ReloadOutlined, HomeOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import { Typography } from 'antd';
import { AlertTriangleIcon } from './Icons';
const { Title, Paragraph } = Typography;

interface Props {
  children: ReactNode;
  /** Optional fallback to render when an error occurs inside children */
  fallback?: ReactNode;
  /** Called whenever an error is caught */
  onError?: (error: Error, errorInfo: ErrorInfo) => void;
}

interface State {
  hasError: boolean;
  error: Error | null;
  errorInfo: ErrorInfo | null;
}

/**
 * Global error boundary.
 * Catches any uncaught errors in the component tree below it and
 * renders a user-friendly error card instead of crashing the whole app.
 *
 * Usage:
 *   <ErrorBoundary>
 *     <MyPage />
 *   </ErrorBoundary>
 *
 * Or with a custom fallback:
 *   <ErrorBoundary fallback={<MyCustomErrorUI />}>
 *     <MyPage />
 *   </ErrorBoundary>
 */
export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null, errorInfo: null };
  }

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    this.setState({ errorInfo });
    this.props.onError?.(error, errorInfo);
    // Also log to console in development
    if (import.meta.env.DEV) {
      console.error('[ErrorBoundary]', error, errorInfo);
    }
  }

  private handleReload = () => {
    this.setState({ hasError: false, error: null, errorInfo: null });
    window.location.reload();
  };

  render() {
    if (this.state.hasError) {
      if (this.props.fallback) return this.props.fallback;
      return <ErrorCard error={this.state.error} onReload={this.handleReload} />;
    }
    return this.props.children;
  }
}

function ErrorCard({
  error,
  onReload,
}: {
  error: Error | null;
  onReload: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div
      style={{
        minHeight: '100%',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        padding: 24,
        background: 'var(--bg-void)',
      }}
    >
      <div
        style={{
          maxWidth: 560,
          background: 'var(--bg-surface)',
          border: '1px solid var(--border-subtle)',
          borderRadius: 12,
          padding: 40,
          textAlign: 'center',
        }}
      >
        <div style={{ marginBottom: 16, display: 'flex', justifyContent: 'center' }}>
          <AlertTriangleIcon size="xxl" color="var(--color-error)" />
        </div>
        <Title level={4} style={{ marginBottom: 8, color: 'var(--text-primary)' }}>
          {t('common.pageError')}
        </Title>
        <Paragraph style={{ color: 'var(--text-secondary)', marginBottom: 24 }}>
          {error?.message || t('errors.unknown')}
        </Paragraph>
        {import.meta.env.DEV && error?.stack && (
          <details
            style={{
              textAlign: 'left',
              marginBottom: 24,
              padding: 12,
              background: 'var(--bg-elevated)',
              borderRadius: 6,
              fontSize: 12,
              fontFamily: 'var(--font-code)',
              color: 'var(--text-secondary)',
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-all',
              maxHeight: 200,
              overflow: 'auto',
            }}
          >
            <summary style={{ cursor: 'pointer', marginBottom: 8, fontSize: 13, color: 'var(--text-muted)' }}>
              {t('common.error')}
            </summary>
            {error.stack}
          </details>
        )}
        <Space size={12}>
          <Button
            icon={<ReloadOutlined />}
            onClick={onReload}
            style={{ borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
          >
            {t('common.retry')}
          </Button>
          <Button
            icon={<HomeOutlined />}
            onClick={() => (window.location.href = '/')}
            style={{ borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
          >
            {t('common.home')}
          </Button>
        </Space>
      </div>
    </div>
  );
}
