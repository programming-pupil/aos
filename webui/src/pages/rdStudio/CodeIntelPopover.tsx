import { Alert, Popover, Space, Tag, Typography } from 'antd';
import { InfoCircleOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { RdCodeIntelQueryResponse, RdCodeIntelStatusResponse } from '@/types';

const { Text } = Typography;

export function CodeIntelPopover({
  status,
  lastResult,
}: {
  status?: RdCodeIntelStatusResponse | null;
  lastResult?: RdCodeIntelQueryResponse | null;
}) {
  const { t } = useTranslation();
  const installedCount = status?.languages.filter((item) => item.installed).length ?? 0;
  const content = (
    <div className="rd-code-intel-popover">
      <Space direction="vertical" size={8} style={{ width: '100%' }}>
        <Text strong>{t('rd.codeIntel', '代码智能')}</Text>
        <Text type="secondary">
          {status
            ? t('rd.codeIntelInstalledCount', '{{count}} 个 language server 可用，未连接时自动回退 symbol/rg。', { count: installedCount })
            : t('rd.codeIntelStatusUnknown', '暂未加载代码智能状态。')}
        </Text>
        {lastResult?.message ? (
          <Alert
            type={lastResult.status === 'ok' ? 'success' : lastResult.status === 'not_found' ? 'info' : 'warning'}
            showIcon
            message={lastResult.message}
          />
        ) : null}
        <div className="rd-code-intel-language-grid">
          {(status?.languages ?? []).map((item) => (
            <span key={item.language} className="rd-code-intel-language">
              <Tag color={item.installed ? 'green' : 'default'}>{item.language}</Tag>
              <Text type="secondary">{item.installed ? item.status : t('rd.notInstalled', '未安装')}</Text>
            </span>
          ))}
        </div>
      </Space>
    </div>
  );
  return (
    <Popover placement="bottomRight" content={content} trigger="click">
      <Tag className="rd-code-intel-status" color={installedCount > 0 ? 'blue' : 'default'}>
        <InfoCircleOutlined /> {t('rd.codeIntelShort', 'Code Intel')}
      </Tag>
    </Popover>
  );
}
