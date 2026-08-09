import i18n from 'i18next';
import type { InitOptions } from 'i18next';
import { initReactI18next } from 'react-i18next';
import LanguageDetector from 'i18next-browser-languagedetector';

import zhCN from './locales/zh-CN.json';
import enUS from './locales/en-US.json';

const resources = {
  'zh-CN': { translation: zhCN },
  'en-US': { translation: enUS },
};

const i18nOptions: InitOptions = {
    resources,
    fallbackLng: 'zh-CN',
    debug: import.meta.env.DEV,
    interpolation: {
      escapeValue: false,
    },
    missingKeyHandler: (lng, ns, key) => {
      if (import.meta.env.DEV) {
        console.warn(`[i18next] missingKey: ${lng} ${ns} ${key}`);
      }
    },
  };

if (!i18n.isInitialized) {
  void i18n
    .use(LanguageDetector)
    .use(initReactI18next)
    .init(i18nOptions);
} else if (import.meta.hot) {
  i18n.addResourceBundle('zh-CN', 'translation', zhCN, true, true);
  i18n.addResourceBundle('en-US', 'translation', enUS, true, true);
}

export const changeLanguage = async (lang: string) => {
  const normalized = lang.toLowerCase().startsWith('en') ? 'en-US' : 'zh-CN';
  await i18n.changeLanguage(normalized);
  localStorage.setItem('i18nextLng', normalized);
  localStorage.setItem('aos-language', normalized);
};

export default i18n;
