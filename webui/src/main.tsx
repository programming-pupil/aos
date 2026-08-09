import React, { useEffect, useState } from 'react';
import ReactDOM from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { Router } from '@/router';
import { ConfigProvider, theme, App as AntApp } from 'antd';
import type { Locale } from 'antd/es/locale';
import App from './App';
import { QueryProvider } from './queryClient';
import { ErrorBoundary } from './components/ErrorBoundary';
import i18n from './i18n';
import './index.css';

document.title = 'AOS — 云端 Agent 开发平台';

// Lazy-load antd locales only when needed
function useAntdLocale() {
  const [locale, setLocale] = useState<Locale | undefined>(undefined);

  useEffect(() => {
    let cancelled = false;

    const normalizeLanguage = (lang?: string | null): 'en-US' | 'zh-CN' => {
      const value = (lang ?? '').trim().toLowerCase();
      if (value.startsWith('en')) return 'en-US';
      if (value.startsWith('zh')) return 'zh-CN';
      return 'zh-CN';
    };

    const loadLocale = async () => {
      const lang = normalizeLanguage(
        i18n.resolvedLanguage ||
          i18n.language ||
          localStorage.getItem('i18nextLng') ||
          localStorage.getItem('aos-language') ||
          'zh-CN',
      );
      try {
        let mod: { default: Locale } | null = null;
        if (lang === 'en-US') {
          mod = await import('antd/locale/en_US');
        } else {
          mod = await import('antd/locale/zh_CN');
        }
        if (!cancelled && mod) {
          setLocale(mod.default);
        }
      } catch (e) {
        // Ignore locale load errors
      }
    };

    loadLocale();

    const handleStorage = () => loadLocale();
    window.addEventListener('storage', handleStorage);
    i18n.on('languageChanged', loadLocale);

    return () => {
      cancelled = true;
      window.removeEventListener('storage', handleStorage);
      i18n.off('languageChanged', loadLocale);
    };
  }, []);

  return locale;
}

function Inner() {
  const antdLocale = useAntdLocale();

  return (
    <ConfigProvider
      locale={antdLocale}
      theme={{
        algorithm: theme.darkAlgorithm,
        token: {
          colorPrimary: '#7c3aed',
          colorBgBase: '#0d1117',
          colorBgContainer: '#161b22',
          colorBgElevated: '#1c2128',
          colorBorder: '#30363d',
          colorBorderSecondary: '#21262d',
          colorText: '#e6edf3',
          colorTextSecondary: '#8b949e',
          colorTextTertiary: '#484f58',
          colorSuccess: '#3fb950',
          colorWarning: '#d29922',
          colorError: '#f85149',
          colorInfo: '#58a6ff',
          borderRadius: 6,
          fontFamily: "Inter, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
          fontFamilyCode: "'JetBrains Mono', 'Fira Code', monospace",
        },
        components: {
          Menu: {
            darkItemBg: 'transparent',
            darkItemSelectedBg: 'rgba(124, 58, 237, 0.15)',
            darkItemHoverBg: '#1c2128',
            darkItemColor: '#8b949e',
            darkItemSelectedColor: '#e6edf3',
          },
          Layout: {
            siderBg: '#0d1117',
            headerBg: '#0d1117',
            bodyBg: '#080c14',
          },
          Button: {
            primaryShadow: '0 2px 8px rgba(124, 58, 237, 0.3)',
          },
          Input: {
            colorBgContainer: '#161b22',
            colorBorder: '#30363d',
            activeBorderColor: '#7c3aed',
            hoverBorderColor: '#484f58',
          },
          Select: {
            colorBgContainer: '#161b22',
            colorBorder: '#30363d',
            optionSelectedBg: 'rgba(124, 58, 237, 0.15)',
          },
          Table: {
            colorBgContainer: '#0d1117',
            headerBg: '#161b22',
            rowHoverBg: '#1c2128',
          },
          Modal: {
            contentBg: '#161b22',
            headerBg: '#161b22',
          },
          Drawer: {
            colorBgElevated: '#161b22',
          },
        },
      }}
    >
      <AntApp>
        <App />
      </AntApp>
    </ConfigProvider>
  );
}

declare global {
  interface Window {
    __AOS_REACT_ROOT__?: Root;
  }
}

const rootElement = document.getElementById('root');
if (!rootElement) {
  throw new Error('Application root element is missing');
}

// Vite can re-evaluate the entry module when a non-component dependency changes.
// Reuse the existing root so HMR never mounts two React trees into the same node.
const root = window.__AOS_REACT_ROOT__ ?? ReactDOM.createRoot(rootElement);
if (import.meta.hot) {
  window.__AOS_REACT_ROOT__ = root;
}

root.render(
  <React.StrictMode>
    <ErrorBoundary>
      <Router>
        <QueryProvider>
          <Inner />
        </QueryProvider>
      </Router>
    </ErrorBoundary>
  </React.StrictMode>,
);
