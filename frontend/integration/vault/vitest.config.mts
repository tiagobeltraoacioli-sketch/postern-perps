// The vault module against the REAL PerpsVault on a local anvil (chain 700770). Not part of `npm test`: it needs a
// running anvil and a forge build. Run through integration/vault/run.mjs. UNAUDITED prototype, loopback only.
import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  root: fileURLToPath(new URL('../..', import.meta.url)),
  test: { environment: 'node', include: ['integration/vault/**/*.test.ts'], testTimeout: 420_000, hookTimeout: 240_000, fileParallelism: false },
});
