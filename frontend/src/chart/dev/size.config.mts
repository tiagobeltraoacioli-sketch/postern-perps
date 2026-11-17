// Size of the chart module alone (React external): npx vite build --config src/chart/dev/size.config.mts --outDir <scratch>
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  plugins: [react()],
  build: {
    target: 'es2022',
    emptyOutDir: true,
    lib: { entry: fileURLToPath(new URL('../index.ts', import.meta.url)), formats: ['es'], fileName: 'chart' },
    rollupOptions: { external: ['react', 'react/jsx-runtime', 'react-dom'] },
  },
});
