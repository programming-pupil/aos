import { Alert, Tabs } from 'antd';
import { NodeIndexOutlined } from '@ant-design/icons';
import { useTranslation } from 'react-i18next';
import { ForeignKeysTab } from '@/components/nl2sql/ForeignKeysTab';
import { JoinPathsTab } from '@/components/nl2sql/JoinPathsTab';

export function RelationshipModelingTab() {
  const { t } = useTranslation();

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
      <Alert
        showIcon
        type="info"
        icon={<NodeIndexOutlined />}
        message={t('management.relationshipModeling.title')}
        description={t('management.relationshipModeling.description')}
      />
      <Tabs
        defaultActiveKey="foreign-keys"
        items={[
          {
            key: 'foreign-keys',
            label: t('management.relationshipModeling.definition'),
            children: <ForeignKeysTab />,
          },
          {
            key: 'join-paths',
            label: t('management.relationshipModeling.paths'),
            children: <JoinPathsTab />,
          },
        ]}
      />
    </div>
  );
}
