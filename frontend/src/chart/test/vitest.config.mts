// The chart's own test configuration: runs only src/chart/test, independent of the app's vite.config.ts.
// Run from frontend/:  npx vitest run --config src/chart/test/vitest.config.mts
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';

const frontend = fileURLToPath(new URL('../../..', import.meta.url));

export default defineConfig({
  root: frontend,
  plugins: [react()],
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/chart/test/setup.ts'],
    restoreMocks: true,
    include: ['src/chart/test/**/*.test.{ts,tsx}'],
    css: false,
  },
});
