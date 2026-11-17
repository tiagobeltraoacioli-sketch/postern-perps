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
    environment: 'jsdom',
    environmentOptions: { jsdom: { url: 'https://perps.invalid/' } },
    setupFiles: ['./src/test/setup.ts'],
    restoreMocks: true,
    include: ['src/test/**/*.test.{ts,tsx}'],
  },
}));
