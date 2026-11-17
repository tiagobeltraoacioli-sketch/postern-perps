// Development and measurement harness of the chart (not part of the app build). SYNTHETIC data only.
// Build: from frontend/, npx vite build --config src/chart/dev/vite.config.mts --outDir <scratch>
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  root: fileURLToPath(new URL('.', import.meta.url)),
  base: './',
  plugins: [react()],
  build: { target: 'es2022', sourcemap: false, emptyOutDir: true },
});
