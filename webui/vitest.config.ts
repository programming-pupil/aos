import { defineConfig } from 'vitest/config';
import path from 'path';

// Vitest configuration for the webui project. Kept separate from vite.config.ts
// so the app build pipeline is unaffected. Resolves the `@` alias the same way
// the app does so tests can import via `@/...` if needed.
export default defineConfig({
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  test: {
    environment: 'node',
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
    setupFiles: ['./src/test/setup.ts'],
    globals: false,
  },
});
