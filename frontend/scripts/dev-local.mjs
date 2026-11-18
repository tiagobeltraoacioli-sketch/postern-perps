#!/usr/bin/env node
// Starts the frontend dev server against a LOCAL stack (loopback only). UNAUDITED prototype; never a deployment.
//
//   node scripts/dev-local.mjs --api http://127.0.0.1:49900 --l2 http://127.0.0.1:49901 \
//        --vault 0x… --token 0x… [--deploy-block N] [--chain 700770] [--port 49920] [--config-out FILE]
//
// Writes a build configuration shaped like config/production.json (default: .local/stack.json, git-ignored) and runs
// `vite --host 127.0.0.1 --port P --strictPort` with POSTERN_PERPS_CONFIG pointing at it. Such a build is a "local
// render" build: it shows the "not for publication" banner and fails check-production-bundle. Every URL must be a
// loopback URL: anything else is refused (this script can never point a dev build at a public host).
// The server's PID is this process (vite runs in it): stop it by PID (SIGTERM).
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const frontend = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const fail = (text) => {
  console.error(`dev-local: ${text}`);
  process.exit(2);
};
const loopback = (name, value) => {
  let url;
  try {
    url = new URL(value);
  } catch {
    fail(`--${name} is not a URL`);
  }
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)) fail(`--${name} must be an http loopback URL (got ${url.origin})`);
  return url.origin;
};
const address = (name, value) => {
  if (value === undefined) return null;
  if (!/^0x[0-9a-fA-F]{40}$/.test(value) || /^0x0{40}$/.test(value)) fail(`--${name} is not a contract address`);
  return value.toLowerCase();
};

const api = loopback('api', opt('api') ?? fail('--api is required'));
const l2 = loopback('l2', opt('l2') ?? fail('--l2 is required'));
const chain = Number(opt('chain', '700770'));
const port = Number(opt('port', '49920'));
if (!Number.isInteger(chain) || chain <= 0) fail('--chain is not a chain id');
if (!Number.isInteger(port) || port < 1024 || port > 65535) fail('--port is not a port');
const config = {
  note: 'LOCAL STACK (loopback). Not for publication. UNAUDITED prototype.',
  apiUrl: api,
  wsUrl: `${api.replace(/^http/, 'ws')}/v1/ws`,
  l2RpcUrl: l2,
  l2ChainId: chain,
  vault: address('vault', opt('vault')),
  collateral: address('token', opt('token')),
  explorerTx: null,
  ...(opt('deploy-block') !== undefined ? { vaultDeployBlock: Number(opt('deploy-block')) } : {}),
};
const out = resolve(opt('config-out', resolve(frontend, '.local/stack.json')));
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, `${JSON.stringify(config, null, 2)}\n`);
process.env.POSTERN_PERPS_CONFIG = out;
console.log(`dev-local: config ${out}; pid ${process.pid}; http://127.0.0.1:${port}/`);

const { createServer } = await import('vite');
const server = await createServer({ root: frontend, configFile: resolve(frontend, 'vite.config.ts'), server: { host: '127.0.0.1', port, strictPort: true } });
await server.listen();
const stop = async () => {
  await server.close();
  process.exit(0);
};
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
