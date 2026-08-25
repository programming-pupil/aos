import { Select } from 'antd';
import { useTranslation } from 'react-i18next';
import { useState, useEffect } from 'react';
import { changeLanguage } from '@/i18n';

const languages = [
  { value: 'zh-CN', label: '中文' },
  { value: 'en-US', label: 'English' },
];

export function LanguageSwitcher() {
  const { i18n } = useTranslation();
  const [value, setValue] = useState(i18n.language);

  useEffect(() => {
    setValue(i18n.language);
  }, [i18n.language]);

  return (
    <Select
      value={value}
      options={languages}
      size="small"
      variant="borderless"
      className="aos-language-switcher"
      style={{ width: 80, color: 'var(--text-primary)' }}
      onChange={(val) => {
        setValue(val);
        changeLanguage(val);
      }}
      popupMatchSelectWidth={false}
    />
  );
}
