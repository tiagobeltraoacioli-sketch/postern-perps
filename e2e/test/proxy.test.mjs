// The L2 pass-through proxy alters exactly what each fault mode says and nothing else (python3 stdlib; no network
// beyond loopback). UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const UP = 49990, PROXY = 49991;
const LOG = { address: '0xaa', data: '0x' + '00'.repeat(31) + '05' + 'ff'.repeat(32), topics: [] };
const BLOCK = { number: '0x10', hash: '0x' + 'ab'.repeat(32) };
const call = async (method, port = PROXY) => (await fetch(`http://127.0.0.1:${port}`, { method: 'POST', body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params: [] }) }));
const fault = async (mode) => (await fetch(`http://127.0.0.1:${PROXY}/__fault`, { method: 'POST', body: JSON.stringify({ mode }) }));

test('proxy fault modes', async (t) => {
  const upstream = createServer((req, res) => {
    let b = '';
    req.on('data', (d) => (b += d)).on('end', () => {
      const m = JSON.parse(b).method;
      const result = m === 'eth_getLogs' ? [LOG] : m === 'eth_getBlockByNumber' ? BLOCK : '0xab162';
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ jsonrpc: '2.0', id: 1, result }));
    });
  }).listen(UP, '127.0.0.1');
  const proxy = spawn('python3', [join(HERE, '..', 'host', 'rpc-proxy.py'), `http://127.0.0.1:${UP}`, String(PROXY)], { stdio: 'ignore' });
  t.after(() => { proxy.kill(); upstream.close(); });
  for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${PROXY}/__fault`); break; } catch { await new Promise((r) => setTimeout(r, 100)); } }

  assert.deepEqual((await (await call('eth_getLogs')).json()).result, [LOG], 'none: pass-through');
  assert.deepEqual((await (await call('eth_getBlockByNumber')).json()).result, BLOCK);

  assert.equal((await fault('amount')).status, 200);
  const a = (await (await call('eth_getLogs')).json()).result[0];
  assert.equal(a.data, '0x' + '00'.repeat(31) + '06' + 'ff'.repeat(32), 'amount: first word + 1, the rest untouched');
  assert.deepEqual((await (await call('eth_getBlockByNumber')).json()).result, BLOCK, 'amount: blocks untouched');

  await fault('drop');
  assert.deepEqual((await (await call('eth_getLogs')).json()).result, []);

  await fault('hash');
  const h = (await (await call('eth_getBlockByNumber')).json()).result;
  assert.notEqual(h.hash, BLOCK.hash);
  assert.equal(h.number, BLOCK.number);
  assert.deepEqual((await (await call('eth_getLogs')).json()).result, [LOG], 'hash: logs untouched');

  await fault('down');
  assert.equal((await call('eth_chainId')).status, 503);

  assert.equal((await fault('nonsense')).status, 400, 'an unknown mode is refused');
  await fault('none');
  assert.equal((await (await call('eth_chainId')).json()).result, '0xab162');
  const st = await (await fetch(`http://127.0.0.1:${PROXY}/__fault`)).json();
  assert.equal(st.mode, 'none');
  assert.ok(st.altered >= 3);
});
