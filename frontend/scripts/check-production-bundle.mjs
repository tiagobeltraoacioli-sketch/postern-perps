#!/usr/bin/env node
// Release gate for a built perps site: node scripts/check-production-bundle.mjs <dist>
// (adapted from postern-dex frontend/scripts/check-production-bundle.mjs). Fails on loopback or private endpoints,
// project hosts, test keys or mnemonics, source maps, a local render build, a harness wallet, a CSP that allows a
// script origin other than 'self', or a connect-src that misses an endpoint of the committed config.
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { basename, join } from 'node:path';

const [dist] = process.argv.slice(2);
if (!dist) {
  console.error('usage: check-production-bundle.mjs <dist>');
  process.exit(2);
}
const TEST_VALUES = [
  'ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80',
  '59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d',
  'f39fd6e51aad88f6f4ce6ab8827279cfffb92266',
  '70997970c51812dc3a010c7d01b50e0d17dc79c8',
];
const FORBIDDEN = [
  ['localhost', /localhost/gi],
  ['loopback', /127\.0\.0\.1|\[::1\]|0\.0\.0\.0/g],
  ['private address', /\b(?:10\.\d+\.\d+\.\d+|192\.168\.\d+\.\d+)\b/g],
  ['project host', /45\.77\.\d+\.\d+|136\.244\.93\.186|209\.250\.236\.240|80\.240\.16\.185|199\.247\.22\.8|139\.180\.\d+\.\d+|140\.82\.35\.231/g],
  ['test mnemonic', /test test test test/g],
  ['key material name', /PRIVATE_KEY|privateKey\s*:\s*["'`]0x/g],
  ['build path', /\/home\/ubuntu|\/Users\/[a-z]/g],
  ['harness wallet', /Harness wallet|__walletBridge/g],
  ['local render build', /not for publication/gi],
  ['mock endpoint', /\/mock\/scenario|X-Mock-Country/gi],
];
const walk = (dir) => readdirSync(dir).flatMap((name) => (statSync(join(dir, name)).isDirectory() ? walk(join(dir, name)) : [join(dir, name)]));
const hits = [];
const files = walk(dist);
for (const path of files) {
  const name = basename(path);
  if (name.endsWith('.map')) hits.push([name, 'source map', '']);
  if (!/\.(js|css|html|json|svg|txt)$|_headers$/.test(path)) continue;
  const text = readFileSync(path, 'utf8');
  for (const v of TEST_VALUES) if (text.toLowerCase().includes(v)) hits.push([name, 'test key or address', v.slice(0, 12)]);
  for (const [label, pattern] of FORBIDDEN) for (const m of text.matchAll(pattern)) hits.push([name, label, text.slice(Math.max(0, m.index - 40), m.index + 40)]);
}
const headers = readFileSync(join(dist, '_headers'), 'utf8');
const csp = headers.match(/Content-Security-Policy: (.*)/)?.[1] ?? '';
if (!/script-src 'self';/.test(csp)) hits.push(['_headers', "script-src is not exactly 'self'", csp]);
if (/unsafe-eval|\*/.test(csp)) hits.push(['_headers', 'a wildcard or unsafe-eval in the CSP', csp]);
const config = JSON.parse(readFileSync(new URL('../config/production.json', import.meta.url), 'utf8'));
const connect = (csp.match(/connect-src ([^;]*)/)?.[1] ?? '').split(' ');
for (const url of [config.apiUrl, config.wsUrl, config.l2RpcUrl]) if (!connect.includes(new URL(url).origin)) hits.push(['_headers', 'endpoint missing from connect-src', url]);
for (const [file, label, context] of hits) console.log(`HIT ${file}: ${label}: ${context.replace(/\s+/g, ' ')}`);
console.log(`${hits.length} hit(s) in ${files.length} files of ${dist}`);
process.exit(hits.length ? 1 : 0);
