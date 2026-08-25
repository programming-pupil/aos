import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

const apiProxyTarget = process.env.AOS_DEMO_API_PROXY_TARGET?.trim() || 'http://localhost:3001';

export default defineConfig({
  plugins: [
    // The entry module owns the React root and must be reloaded as a document,
    // not evaluated as a Fast Refresh boundary.
    react({ exclude: /[/\\]src[/\\]main\.tsx$/ }),
    {
      name: 'vite-plugin-spa-fallback',
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          const url = req.url ?? '';
          if (!url.startsWith('/api') && !url.startsWith('/@') && !url.includes('.') && !url.includes('?')) {
            req.url = '/';
          }
          next();
        });
      },
    },
  ],
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  optimizeDeps: {
    include: ['monaco-editor'],
  },
  build: {
    chunkSizeWarningLimit: 1500,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes('node_modules')) return undefined;
          if (
            id.includes('/react/')
            || id.includes('/react-dom/')
            || id.includes('/wouter/')
            || id.includes('/scheduler/')
          ) {
            return 'vendor-react';
          }
          if (
            id.includes('/antd/')
            || id.includes('/@ant-design/')
            || id.includes('/rc-')
            || id.includes('/dayjs/')
          ) {
            return 'vendor-antd';
          }
          if (id.includes('/echarts')) {
            return 'vendor-charts';
          }
          if (id.includes('/write-excel-file/') || id.includes('/xlsx/') || id.includes('/cfb/') || id.includes('/codepage/') || id.includes('/crc-32/')) {
            return 'vendor-excel';
          }
          if (id.includes('/monaco-editor/')) {
            return 'vendor-monaco';
          }
          if (
            id.includes('/@tanstack/')
            || id.includes('/axios/')
            || id.includes('/i18next/')
            || id.includes('/react-i18next/')
            || id.includes('/zustand/')
          ) {
            return 'vendor-core';
          }
          return undefined;
        },
      },
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: apiProxyTarget,
        changeOrigin: true,
      },
      '/ws': {
        target: apiProxyTarget,
        ws: true,
        changeOrigin: true,
      },
    },
  },
  worker: {
    format: 'es',
  },
});
