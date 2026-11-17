#!/usr/bin/env node
// A loopback stand-in of the Bloch L2 JSON-RPC that serves the vault's Deposit events, so a dev node's watcher can
// credit the conformance suite's --funded account without an L2. UNAUDITED prototype, development and tests only:
// it answers only eth_chainId, eth_getBlockByNumber and eth_getLogs, from a fixed chain the operator describes.
//
//   node node/mock/conformance/fake-l2.mjs --port 49361 --vault 0x... --deposit 0xACCOUNT:AMOUNT_USDC [--deposit ...]
//
// The node requires TWO endpoints that agree: start this twice on two ports with the same arguments. Deposit
// events follow docs/VAULT.md 0.2: `Deposit(uint64 indexed index, address indexed account, address indexed sender,
// uint64 amount, bytes32 head)`, head chained per VAULT.md 5.4 from head0 = keccak(abi.encode(bytes32
// "PosternPerps.deposit.v1", 700770, vault)). Block n (1..N) holds deposit n-1; the finalized block is N.
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { keccak256, hex, unhex, concat } from '../lib/keccak.mjs';
import { parseFixed } from '../lib/fixed.mjs';

const enc = new TextEncoder();
const word = (v) => {
  const b = new Uint8Array(32);
  let x = BigInt(v);
  for (let i = 31; i >= 0; i--) { b[i] = Number(x & 255n); x >>= 8n; }
  return b;
};
const tag32 = (t) => { const b = new Uint8Array(32); b.set(enc.encode(t)); return b; };
const q = (n) => '0x' + BigInt(n).toString(16);

export function fakeChain({ vault, deposits, chainId = 700770, startTime = 1794878400 }) {
  vault = vault.toLowerCase();
  let head = keccak256(concat(tag32('PosternPerps.deposit.v1'), word(chainId), word(BigInt(vault))));
  const sig = hex(keccak256(enc.encode('Deposit(uint64,address,address,uint64,bytes32)')));
  const blocks = [{ number: 0, hash: hex(keccak256(enc.encode('fake-l2/0'))), timestamp: startTime }];
  const logs = [];
  deposits.forEach(({ account, amount }, i) => {
    const n = i + 1;
    const a = BigInt(account.toLowerCase());
    head = keccak256(concat(head, word(i), word(a), word(amount)));
    const hash = hex(keccak256(enc.encode(`fake-l2/${n}/${hex(head)}`)));
    blocks.push({ number: n, hash, timestamp: startTime + 2 * n });
    logs.push({
      address: vault, blockNumber: q(n), blockHash: hash, transactionHash: hex(keccak256(enc.encode(`fake-l2/tx/${n}`))),
      logIndex: '0x0', transactionIndex: '0x0', removed: false,
      topics: [sig, hex(word(i)), hex(word(a)), hex(word(a))],
      data: hex(concat(word(amount), head)),
    });
  });
  return { chainId, blocks, logs, finalized: blocks.length - 1 };
}

export function serveFake(chain, port) {
  const block = (b) => (b ? { number: q(b.number), hash: b.hash, timestamp: q(b.timestamp), parentHash: '0x' + '00'.repeat(32) } : null);
  const server = createServer(async (req, res) => {
    let body = '';
    for await (const c of req) body += c;
    let r;
    try {
      const j = JSON.parse(body);
      const p = j.params ?? [];
      let result = null;
      if (j.method === 'eth_chainId') result = q(chain.chainId);
      else if (j.method === 'eth_getBlockByNumber') {
        const t = p[0];
        const n = t === 'finalized' || t === 'latest' || t === 'safe' ? chain.finalized : Number(BigInt(t));
        result = block(chain.blocks[n]);
      } else if (j.method === 'eth_getLogs') {
        const f = p[0] ?? {};
        const from = Number(BigInt(f.fromBlock ?? '0x0')), to = Number(BigInt(f.toBlock ?? q(chain.finalized)));
        result = chain.logs.filter((l) => { const b = Number(BigInt(l.blockNumber)); return b >= from && b <= to && (!f.address || f.address.toLowerCase() === l.address); });
      } else {
        r = { jsonrpc: '2.0', id: j.id, error: { code: -32601, message: 'method not served by fake-l2' } };
      }
      r ??= { jsonrpc: '2.0', id: j.id, result };
    } catch (e) {
      r = { jsonrpc: '2.0', id: null, error: { code: -32700, message: String(e.message) } };
    }
    const out = JSON.stringify(r);
    res.writeHead(200, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(out) });
    res.end(out);
  });
  return new Promise((ok) => server.listen(port, '127.0.0.1', () => ok(server)));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const opt = (n) => args[args.indexOf(`--${n}`) + 1];
  const deposits = [];
  args.forEach((a, i) => {
    if (a === '--deposit') {
      const [account, amt] = args[i + 1].split(':');
      deposits.push({ account, amount: parseFixed(amt, 6) });
    }
  });
  const chain = fakeChain({ vault: opt('vault'), deposits });
  await serveFake(chain, Number(opt('port')));
  process.stdout.write(`fake-l2 (UNAUDITED, dev only) on 127.0.0.1:${opt('port')}  vault ${opt('vault')}  deposits ${deposits.length}  pid ${process.pid}\n`);
}
