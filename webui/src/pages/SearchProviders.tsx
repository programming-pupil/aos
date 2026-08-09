import { Alert, Typography } from 'antd';
import { useTranslation } from 'react-i18next';
import { PmSearchWorkbenchPanel } from './operations/PmSearchWorkbenchPanel';

const { Title, Text } = Typography;

export default function SearchProviders() {
  const { t } = useTranslation();

  return (
    <div
      style={{
        height: '100%',
        minHeight: 0,
        padding: 24,
        background: 'var(--bg-surface)',
        overflow: 'auto',
      }}
    >
      <div style={{ maxWidth: 1120, margin: '0 auto', height: '100%', minHeight: 0 }}>
        <div style={{ marginBottom: 16 }}>
          <Title level={3} style={{ margin: 0 }}>
            {t('nav.searchProviders', 'Search Extensions')}
          </Title>
          <Text type="secondary">
            {t(
              'operations.searchProvidersPageDescription',
              'AOS built-in web search works without an API key. Configure optional Search Extensions to improve coverage, reliability, and private-source access.',
            )}
          </Text>
        </div>
        <Alert
          type="success"
          showIcon
          style={{ marginBottom: 16 }}
          message={t('operations.builtinSearchEnabled', 'AOS built-in web search is enabled')}
          description={t(
            'operations.builtinSearchDescription',
            'It uses live public web sources without a search API key. Search Extensions remain optional enhancements and are tried first when configured.',
          )}
        />
        <div style={{ minHeight: 520 }}>
          <PmSearchWorkbenchPanel variant="page" />
        </div>
      </div>
    </div>
  );
}
