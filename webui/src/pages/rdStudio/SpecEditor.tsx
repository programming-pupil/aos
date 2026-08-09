import { Input, Segmented, Space, Typography } from 'antd';
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Markdown } from '@/components/chat';

const { Text } = Typography;

export function SpecEditor({
  title,
  value,
  placeholder,
  onChange,
}: {
  title: string;
  value?: string | null;
  placeholder?: string;
  onChange?: (value: string) => void;
}) {
  const { t } = useTranslation();
  const [view, setView] = useState<'preview' | 'edit'>('preview');
  const content = useMemo(() => value?.trim() || '', [value]);

  return (
    <div className="rd-plan-editor">
      <div className="rd-plan-editor-toolbar">
        <Text strong>{title}</Text>
        <Segmented
          size="small"
          value={view}
          onChange={(next) => setView(next as 'preview' | 'edit')}
          options={[
            { label: t('rd.preview', '预览'), value: 'preview' },
            { label: t('rd.edit', '编辑'), value: 'edit' },
          ]}
        />
      </div>
      {view === 'edit' ? (
        <Input.TextArea
          value={content}
          onChange={(event) => onChange?.(event.target.value)}
          placeholder={placeholder}
          autoSize={{ minRows: 12, maxRows: 28 }}
        />
      ) : content ? (
        <div className="rd-plan-markdown">
          <Markdown relaxed>{content}</Markdown>
        </div>
      ) : (
        <Space className="rd-plan-empty">
          <Text type="secondary">{placeholder || t('rd.planDocEmpty', '等待生成内容')}</Text>
        </Space>
      )}
    </div>
  );
}
