import { Button, Drawer, Empty, List, Space, Tag, Typography } from 'antd';
import { AimOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import type { RdCodeIntelLocation } from '@/types';

const { Text } = Typography;

export function DefinitionCandidates({
  open,
  title,
  locations,
  source,
  message,
  onClose,
  onOpenLocation,
}: {
  open: boolean;
  title?: string;
  locations: RdCodeIntelLocation[];
  source?: string | null;
  message?: string | null;
  onClose: () => void;
  onOpenLocation: (location: RdCodeIntelLocation) => void;
}) {
  const { t } = useTranslation();
  return (
    <Drawer
      title={(
        <Space>
          <AimOutlined />
          <span>{title || t('rd.definitionCandidates', '定义候选')}</span>
          {source ? <Tag color={source === 'lsp' ? 'green' : 'gold'}>{source}</Tag> : null}
        </Space>
      )}
      open={open}
      width={520}
      onClose={onClose}
      styles={{
        body: { background: '#020617', padding: 12 },
        header: { background: '#07111f', borderBottomColor: 'rgba(148, 163, 184, 0.18)' },
      }}
    >
      {message ? (
        <Text style={{ display: 'block', marginBottom: 10, color: '#94a3b8' }}>{message}</Text>
      ) : null}
      {locations.length === 0 ? (
        <Empty
          image={Empty.PRESENTED_IMAGE_SIMPLE}
          description={<span style={{ color: '#94a3b8' }}>{t('rd.codeIntelNoResult', '没有找到跳转结果')}</span>}
        />
      ) : (
        <List
          size="small"
          dataSource={locations}
          renderItem={(item) => (
            <List.Item className="rd-code-intel-location">
              <Button type="link" onClick={() => onOpenLocation(item)}>
                <Space direction="vertical" size={2} style={{ minWidth: 0, textAlign: 'left' }}>
                  <Text className="rd-code-intel-location-path">
                    {item.path}:{Math.max(1, item.line + 1)}
                  </Text>
                  {item.preview ? <Text className="rd-code-intel-location-preview">{item.preview}</Text> : null}
                </Space>
              </Button>
            </List.Item>
          )}
        />
      )}
    </Drawer>
  );
}
