import { readFileSync } from 'node:fs';
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

const env =
  (globalThis as { process?: { env: Record<string, string | undefined> } }).process?.env ?? {};
const committed = decodeURIComponent(new URL('./config/production.json', import.meta.url).pathname);
// POSTERN_PERPS_CONFIG points a local render build at a loopback mock (absolute path of a file shaped like
// config/production.json). Such a build shows a "not for publication" banner and fails check-production-bundle.
const configPath = env.POSTERN_PERPS_CONFIG ?? committed;
const config = JSON.parse(readFileSync(configPath, 'utf8'));

export default defineConfig(({ command }) => ({
  plugins: [react()],
  define: {
    __POSTERN_CONFIG__: JSON.stringify(config),
    __POSTERN_LOCAL_RENDER__: JSON.stringify(command === 'serve' || configPath !== committed),
  },
  base: './',
  server: { host: '127.0.0.1' },
  preview: { host: '127.0.0.1' },
  build: { target: 'es2022', sourcemap: false, chunkSizeWarningLimit: 900 },
  test: {
    // Two projects in one run (`vitest run`): the app's tests and the chart's (src/chart/test, agent 396's suite, which
    // needs its own canvas stand-in setup). The chart's standalone config (src/chart/test/vitest.config.mts) still works.
    projects: [
      {
        extends: true,
        test: {
          name: 'app',
          environment: 'jsdom',
          environmentOptions: { jsdom: { url: 'https://perps.invalid/' } },
          setupFiles: ['./src/test/setup.ts'],
          restoreMocks: true,
          include: ['src/test/**/*.test.{ts,tsx}'],
        },
      },
      {
        extends: true,
        test: {
          name: 'chart',
          environment: 'jsdom',
          setupFiles: ['./src/chart/test/setup.ts'],
          restoreMocks: true,
          include: ['src/chart/test/**/*.test.{ts,tsx}'],
          css: false,
        },
      },
    ],
  },
}));
