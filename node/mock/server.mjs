#!/usr/bin/env node
// PosternDEX Perpetuals: reference mock server of docs/API.md v0.1. UNAUDITED prototype, development and tests only.
// Node.js 20 or newer, no dependencies, loopback only. Owner: agent 393 (node/mock/). README.md in this directory.
//
//   node node/mock/server.mjs [--port 49300] [--seed 7] [--start-time 1794878400000] [--static] [--blocks 40]
//                             [--block-ms 200] [--no-rate-limit] [--nonce-window 120000:30000] [--eip712 FILE]
//
// --static: produce the initial blocks, then a block only when an action arrives or on /mock/scenario?step=N
// (screenshots and tests are byte-stable). Scenario switches: GET /mock/scenario?<name>=<value> (README).
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { Exchange, INTERVALS, FEE_ACCOUNT, EngineError } from './lib/exchange.mjs';
import * as W from './lib/wire.mjs';
import { admit, ApiError, geoDecision } from './lib/admission.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '..', '..');
const readJson = (p) => JSON.parse(readFileSync(p, 'utf8'));

export function loadFixtures(opts = {}) {
  return {
    markets: readJson(opts.marketsFile ?? join(HERE, 'fixtures', 'markets.json')),
    eip712: readJson(opts.eip712File ?? join(HERE, 'fixtures', 'eip712.json')),
    geo: readJson(opts.geoFile ?? join(REPO, 'compliance', 'geoblock.json')),
    mockGeo: readJson(opts.mockGeoFile ?? join(HERE, 'fixtures', 'geoblock.mock.json')),
  };
}

const LIMITS = { bodyBytes: 16_384, wsIn: 16_384, wsOutQueue: 1024, actionsPerAccount: [20, 40], actionsPerIp: [50, 100], readsPerIp: [100, 200], wsPerIp: 20, subsPerConn: 100 };

class Bucket {
  constructor(rate, burst) {
    this.rate = rate;
    this.burst = burst;
    this.map = new Map();
  }
  take(key, now) {
    let b = this.map.get(key);
    if (!b) this.map.set(key, (b = { tokens: this.burst, t: now }));
    b.tokens = Math.min(this.burst, b.tokens + ((now - b.t) / 1000) * this.rate);
    b.t = now;
    if (b.tokens < 1) return Math.ceil((1 - b.tokens) / this.rate);
    b.tokens -= 1;
    return 0;
  }
}

export async function startMock(opts = {}) {
  const fx = loadFixtures(opts);
  const blockMs = opts.blockMs ?? 200;
  const x = new Exchange({ seed: opts.seed ?? 7, startTime: opts.startTime ?? 1794878400000, blockMs, startHeight: opts.startHeight ?? 120000, marketsFx: fx.markets, eip712Fx: fx.eip712 });
  const staticMode = !!opts.static;
  const rateLimit = opts.rateLimit !== false;
  const sanctioned = new Set(fx.mockGeo.mock_sanctioned_addresses.map((a) => a.toLowerCase()));
  for (const c of fx.mockGeo.mock_allowed_countries) {
    const e = fx.geo.countries[c];
    if (e && e.tier !== 'allowed') throw new Error(`mock allow-list names ${c}, which compliance/geoblock.json lists as ${e.tier}`);
  }
  const state = { geoDefault: fx.mockGeo.mock_default_country, eip712: 'pinned', refuseSockets: 0, nonceWindowMs: opts.nonceWindowMs ?? null };
  const buckets = { acct: new Bucket(...LIMITS.actionsPerAccount), ip: new Bucket(...LIMITS.actionsPerIp), read: new Bucket(...LIMITS.readsPerIp) };
  const sockets = new Set();
  const waiters = [];

  // ---------- blocks and broadcast ----------
  function step() {
    const ev = x.produceBlock();
    if (ev) broadcast(ev);
    return ev;
  }
  for (let i = 0; i < (opts.blocks ?? 40); i++) step();
  let timer = null;
  const schedule = () => {
    clearInterval(timer);
    timer = staticMode ? null : setInterval(step, x.blockMs);
  };
  schedule();

  // ---------- HTTP ----------
  const err = (e) => ({ error: { code: e.code, message: e.message, details: e.details ?? {} } });
  const headers = { 'content-type': 'application/json', 'access-control-allow-origin': '*', 'access-control-allow-headers': 'content-type, x-mock-country', 'access-control-allow-methods': 'GET, POST, OPTIONS', 'cache-control': 'no-store' };
  const send = (res, code, body, extra = {}) => {
    res.writeHead(code, { ...headers, ...extra });
    res.end(JSON.stringify(body));
  };
  const ipOf = (req) => req.socket.remoteAddress ?? '?';
  const countryOf = (req, url) => {
    const h = req.headers['x-mock-country'] ?? url.searchParams.get('country');
    if (h !== undefined && h !== null) return String(h).toUpperCase();
    return state.geoDefault;
  };
  const geoFor = (country) => geoDecision(fx.geo, fx.mockGeo, country);
  const intParam = (url, name, def, lo, hi) => {
    const v = url.searchParams.get(name);
    if (v === null) return def;
    if (!/^\d+$/.test(v) || Number(v) < lo || Number(v) > hi) throw new ApiError(400, 'MALFORMED', `${name} must be an integer in [${lo}, ${hi}]`, { path: name });
    return Number(v);
  };
  /** Newest-first pagination by a numeric key; `before` exclusive. */
  function page(list, key, url) {
    const limit = intParam(url, 'limit', 100, 1, 1000);
    const before = url.searchParams.get('before');
    let items = list;
    if (before !== null) {
      if (!/^\d+$/.test(before)) throw new ApiError(400, 'MALFORMED', 'before must be a cursor from nextCursor', { path: 'before' });
      const b = BigInt(before);
      items = list.filter((i) => BigInt(key(i)) < b);
    }
    const out = items.slice(0, limit);
    return { out, next: items.length > limit ? String(key(out.at(-1))) : undefined };
  }
  const marketOf = (sym) => {
    const m = x.markets.get(sym);
    if (!m) throw new ApiError(404, 'NOT_FOUND', `unknown market ${sym}`);
    return m;
  };
  const eip712Body = () => {
    const { _note, ...rest } = fx.eip712;
    if (state.eip712 === 'pinned') return rest;
    // "drift": the node publishes typed data the client did not pin (the frontend must refuse to sign).
    const t = JSON.parse(JSON.stringify(rest.types));
    t.CancelOrder = [...t.CancelOrder, { name: 'drift', type: 'uint64' }];
    return { ...rest, types: t };
  };

  async function readBody(req) {
    const chunks = [];
    let n = 0;
    for await (const c of req) {
      n += c.length;
      if (n > LIMITS.bodyBytes) throw new ApiError(413, 'TOO_LARGE', 'body over 16 KiB');
      chunks.push(c);
    }
    try {
      return JSON.parse(Buffer.concat(chunks).toString('utf8'));
    } catch {
      throw new ApiError(400, 'MALFORMED', 'body is not JSON', { path: '' });
    }
  }

  function admitAction(body, country, ip) {
    const now = Date.now();
    if (rateLimit) {
      const r = buckets.ip.take(ip, now);
      if (r) throw new ApiError(429, 'RATE_LIMITED', 'too many actions from this address', { retryAfter: r });
    }
    const a = admit(x, body, { geo: geoFor(country), sanctioned, nonceWindowMs: state.nonceWindowMs, now: x.time });
    if (rateLimit) {
      const r = buckets.acct.take(a.account, now);
      if (r) throw new ApiError(429, 'RATE_LIMITED', 'too many actions for this account', { retryAfter: r });
    }
    x.pending.push(a);
    const ack = { actionHash: a.actionHash, status: 'accepted', receivedAt: a.receivedAt };
    x.acks.set(a.actionHash, ack);
    if (staticMode) queueMicrotask(step); // static mode: a block per action, so tests see inclusion at once
    return { a, ack };
  }

  const routes = [];
  const get = (re, fn) => routes.push([re, fn]);
  get(/^\/v1\/markets$/, () => ({ markets: [...x.markets.values()].map((m) => W.market(x, m)) }));
  get(/^\/v1\/tickers$/, () => ({ tickers: [...x.markets.values()].map((m) => W.ticker(x, m)) }));
  get(/^\/v1\/markets\/([A-Za-z0-9-]+)$/, (u, [s]) => W.market(x, marketOf(s)));
  get(/^\/v1\/markets\/([A-Za-z0-9-]+)\/ticker$/, (u, [s]) => W.ticker(x, marketOf(s)));
  get(/^\/v1\/markets\/([A-Za-z0-9-]+)\/book$/, (u, [s]) => W.book(x, marketOf(s), intParam(u, 'depth', 50, 1, 500)));
  get(/^\/v1\/markets\/([A-Za-z0-9-]+)\/trades$/, (u, [s]) => {
    const p = page(marketOf(s).trades, (t) => t.tradeId, u);
    return { trades: p.out.map(W.trade), ...(p.next ? { nextCursor: p.next } : {}) };
  });
  get(/^\/v1\/markets\/([A-Za-z0-9-]+)\/candles$/, (u, [s]) => {
    const m = marketOf(s);
    const iv = u.searchParams.get('interval') ?? '1m';
    if (!INTERVALS[iv]) throw new ApiError(400, 'MALFORMED', 'interval is one of 1m 5m 15m 1h 4h 1d', { path: 'interval' });
    const limit = intParam(u, 'limit', 300, 1, 1500);
    const from = u.searchParams.has('from') ? intParam(u, 'from', 0, 0, Number.MAX_SAFE_INTEGER) : null;
    const to = u.searchParams.has('to') ? intParam(u, 'to', 0, 0, Number.MAX_SAFE_INTEGER) : null;
    let list = m.candles[iv].filter((c) => (from === null || c.t >= from) && (to === null || c.t < to));
    list = from !== null && to === null ? list.slice(0, limit) : list.slice(-limit);
    return { candles: list.map((c) => W.candle(m, c)) };
  });
  get(/^\/v1\/markets\/([A-Za-z0-9-]+)\/funding$/, (u, [s]) => {
    const m = marketOf(s);
    const p = page(m.funding, (f) => f.time, u);
    return { funding: p.out.map((f) => W.fundingRate(m, f)), ...(p.next ? { nextCursor: p.next } : {}) };
  });
  get(/^\/v1\/oracle\/([A-Za-z0-9-]+)$/, (u, [s]) => W.oracle(x, marketOf(s)));
  get(/^\/v1\/status$/, () => W.status(x, 'mock'));
  get(/^\/v1\/geo$/, (u, _, req) => {
    const g = geoFor(countryOf(req, u));
    const addr = (u.searchParams.get('account') ?? '').toLowerCase();
    return { country: g.country, restricted: !g.allowed, policy: g.policy, listVersion: g.listVersion, reasonCode: g.reason, message: g.allowed ? null : 'PosternDEX Perpetuals is not available in your jurisdiction.', sanctioned: sanctioned.has(addr) };
  });
  get(/^\/v1\/eip712$/, eip712Body);
  get(/^\/v1\/committee$/, () => ({ threshold: x.threshold, validators: x.committee.map((c) => c.address), setHash: x.setHash }));
  get(/^\/v1\/finality\/latest$/, () => {
    const cp = x.checkpoints.at(-1);
    if (!cp) throw new ApiError(404, 'NOT_FOUND', 'no checkpoint yet');
    return W.checkpoint(x, cp);
  });
  get(/^\/v1\/finality\/(\d+)$/, (u, [h]) => {
    const cp = [...x.checkpoints].reverse().find((c) => c.height <= Number(h));
    if (!cp) throw new ApiError(404, 'NOT_FOUND', 'no checkpoint at or before that height');
    return W.checkpoint(x, cp);
  });
  get(/^\/v1\/blocks$/, (u) => {
    const first = Math.min(...x.blocks.keys());
    const from = intParam(u, 'from', first, 0, Number.MAX_SAFE_INTEGER);
    const limit = intParam(u, 'limit', 100, 1, 500);
    const out = [];
    for (let h = Math.max(from, first); h <= x.height && out.length < limit; h++) out.push(W.block(x, x.blocks.get(h)));
    return { blocks: out };
  });
  get(/^\/v1\/blocks\/(\d+)$/, (u, [h]) => {
    const b = x.blocks.get(Number(h));
    if (!b) throw new ApiError(404, 'NOT_FOUND', 'unknown or pruned height');
    return W.block(x, b);
  });
  const ADDR = '(0x[0-9a-fA-F]{40})';
  const acct = (a) => x.accountForRead(a);
  get(new RegExp(`^/v1/accounts/${ADDR}$`), (u, [a]) => W.account(x, acct(a)));
  get(new RegExp(`^/v1/accounts/${ADDR}/orders$`), (u, [a]) => {
    const A = acct(a);
    const st = u.searchParams.get('status') ?? 'open';
    if (st !== 'open' && st !== 'all') throw new ApiError(400, 'MALFORMED', 'status is open or all', { path: 'status' });
    const sym = u.searchParams.get('symbol');
    let list = [...A.orders.values()];
    if (st === 'all') list = [...list, ...A.history];
    if (sym) list = list.filter((o) => o.m.symbol === sym);
    list.sort((p, q) => (p.orderId > q.orderId ? -1 : 1));
    const p = page(list, (o) => o.orderId, u);
    return { orders: p.out.map(W.order), ...(p.next ? { nextCursor: p.next } : {}) };
  });
  get(new RegExp(`^/v1/accounts/${ADDR}/fills$`), (u, [a]) => {
    const sym = u.searchParams.get('symbol');
    const p = page(acct(a).fills.filter((f) => !sym || f.m.symbol === sym), (f) => f.fillId, u);
    return { fills: p.out.map(W.fill), ...(p.next ? { nextCursor: p.next } : {}) };
  });
  get(new RegExp(`^/v1/accounts/${ADDR}/funding$`), (u, [a]) => {
    const sym = u.searchParams.get('symbol');
    const p = page(acct(a).funding.filter((f) => !sym || f.symbol === sym), (f) => f.time, u);
    return { payments: p.out.map(W.fundingPayment), ...(p.next ? { nextCursor: p.next } : {}) };
  });
  get(new RegExp(`^/v1/accounts/${ADDR}/deposits$`), (u, [a]) => ({ deposits: acct(a).deposits.map(W.deposit) }));
  get(new RegExp(`^/v1/accounts/${ADDR}/withdrawals$`), (u, [a]) => ({ withdrawals: acct(a).withdrawals.map((w) => W.withdrawal(x, w)) }));
  get(new RegExp(`^/v1/accounts/${ADDR}/agents$`), (u, [a]) => ({ agents: [...acct(a).agents.values()].filter((g) => g.expiresAt > x.time).map(W.agent) }));
  get(new RegExp(`^/v1/accounts/${ADDR}/nonce$`), (u, [a]) => {
    const A = acct(a);
    const next = (s) => String((x.nonceBySigner.get(s) ?? -1n) + 1n);
    return { account: A.address, nextNonce: next(A.address), agents: [...A.agents.keys()].map((g) => ({ agent: g, nextNonce: next(g) })) };
  });
  get(new RegExp(`^/v1/accounts/${ADDR}/proof$`), (u, [a]) => {
    const p = W.accountProof(x, acct(a));
    if (!p) throw new ApiError(404, 'NOT_FOUND', 'account not in the latest finalized root');
    return p;
  });
  get(/^\/v1\/deposits\/(0x[0-9a-fA-F]{64})$/, (u, [h]) => {
    const out = [];
    for (const A of x.accounts.values()) for (const d of A.deposits) if (d.l2TxHash === h.toLowerCase()) out.push(W.deposit(d));
    if (!out.length) throw new ApiError(404, 'NOT_FOUND', 'unknown L2 transaction');
    return { deposits: out };
  });
  get(/^\/v1\/withdrawals\/(\d+)$/, (u, [id]) => {
    for (const A of x.accounts.values()) for (const w of A.withdrawals) if (String(w.withdrawalId) === id) return W.withdrawal(x, w);
    throw new ApiError(404, 'NOT_FOUND', 'unknown withdrawal');
  });
  get(/^\/v1\/forced$/, (u) => {
    const a = u.searchParams.get('account');
    if (a !== null && !/^0x[0-9a-fA-F]{40}$/.test(a)) throw new ApiError(400, 'MALFORMED', 'account must be an address', { path: 'account' });
    return { forced: [] };
  });
  get(/^\/v1\/actions\/(0x[0-9a-fA-F]{64})$/, (u, [h]) => {
    const ack = x.acks.get(h.toLowerCase());
    if (!ack) throw new ApiError(404, 'NOT_FOUND', 'unknown or pruned action');
    return ack;
  });
  get(/^\/mock\/info$/, () => ({ mock: true, unaudited: true, seed: String(x.seed), demoAccount: x.demo, demoKeyLabel: `postern-perps-mock/demo/${x.seed}`, liquidityAccount: x.liquidity, feeAccount: FEE_ACCOUNT, sequencer: x.sequencer, conservation: (() => { const c = x.conservation(); return { ok: c.ok, sizesZero: c.sizesZero, lhs: String(c.lhs), rhs: String(c.rhs) }; })() }));

  // ---------- scenarios (mock only) ----------
  const SCENARIOS = {
    halt: (v) => { const m = marketOf(v); m.status = 'halted'; m.haltReason = 'Oracle unsafe: fewer than 3 independent sources agree within 2%.'; },
    resume: (v) => { const m = marketOf(v); m.status = 'active'; m.haltReason = null; m.oracleState = 'ok'; },
    closeOnly: (v) => { marketOf(v).status = 'closeOnly'; },
    oracle: (v) => { const [s, st] = v.split(':'); const m = marketOf(s); if (!['ok', 'stale', 'deviation', 'insufficientSources'].includes(st)) throw new ApiError(400, 'MALFORMED', 'oracle=SYMBOL:ok|stale|deviation|insufficientSources'); m.oracleState = st; if (st !== 'ok') { m.status = 'halted'; m.haltReason = `Oracle ${st}.`; } },
    busy: (v) => { x.busy = v === '1'; x.blockMs = x.busy ? 20 : blockMs; schedule(); },
    geo: (v) => { state.geoDefault = v === 'none' ? '' : v.toUpperCase(); },
    sanction: (v) => { sanctioned.add(v.toLowerCase()); },
    unsanction: (v) => { sanctioned.delete(v.toLowerCase()); },
    vault: (v) => { if (!['normal', 'paused', 'escape', 'unknown'].includes(v)) throw new ApiError(400, 'MALFORMED', 'vault=normal|paused|escape|unknown'); x.vaultMode = v; },
    sequencer: (v) => { if (!['running', 'recovering', 'halted'].includes(v)) throw new ApiError(400, 'MALFORMED', 'sequencer=running|recovering|halted'); x.sequencerState = v; },
    finality: (v) => { x.finalityStalled = v === 'stalled'; },
    eip712: (v) => { state.eip712 = v === 'pinned' ? 'pinned' : 'drift'; },
    accounts: (v) => { x.autoDemo = v !== 'empty'; },
    demo: (v) => { x.seedDemo(v.toLowerCase()); },
    deposit: (v) => { const [a, amt] = v.split(':'); x.mockDeposit(a, amt ?? '1000'); },
    disconnect: (v) => { state.refuseSockets = Number(v) || 5; for (const s of sockets) s.socket.destroy(); setTimeout(() => (state.refuseSockets = 0), state.refuseSockets * 1000).unref(); },
    liquidate: (v) => { const m = marketOf(v); const ev = x.newEvents(); for (const A of x.accounts.values()) if (A.address !== x.liquidity && A.positions.get(m.id)?.size) x.liquidate(A, m, ev); broadcast(ev); },
    step: (v) => { for (let i = 0; i < (Number(v) || 1); i++) step(); },
    nonceWindow: (v) => { state.nonceWindowMs = v === 'off' ? null : v.split(':').map(Number); },
  };

  const server = createServer(async (req, res) => {
    let url;
    try {
      url = new URL(req.url, 'http://127.0.0.1');
      if (req.method === 'OPTIONS') return send(res, 204, {});
      const path = url.pathname;
      if (path === '/mock/scenario') {
        for (const [k, v] of url.searchParams) {
          if (!SCENARIOS[k]) throw new ApiError(400, 'MALFORMED', `unknown scenario ${k}`, { known: Object.keys(SCENARIOS) });
          SCENARIOS[k](v);
        }
        broadcastStatus();
        return send(res, 200, { ok: true, height: x.height, time: x.time, geoDefault: state.geoDefault, eip712: state.eip712, vaultMode: x.vaultMode, sequencer: x.sequencerState, finalityStalled: x.finalityStalled, busy: x.busy });
      }
      if (req.method === 'POST' && path === '/v1/actions') {
        const body = await readBody(req);
        const { a, ack } = admitAction(body, countryOf(req, url), ipOf(req));
        if (url.searchParams.get('wait') === 'included') {
          const done = await new Promise((r) => {
            a.resolve = r;
            setTimeout(() => r(null), 2000).unref();
          });
          return send(res, 200, done ?? ack);
        }
        return send(res, 200, ack);
      }
      if (req.method !== 'GET') throw new ApiError(404, 'NOT_FOUND', 'no such endpoint');
      if (rateLimit && path.startsWith('/v1/')) {
        const r = buckets.read.take(ipOf(req), Date.now());
        if (r) return send(res, 429, err(new ApiError(429, 'RATE_LIMITED', 'too many reads')), { 'retry-after': String(r) });
      }
      if (path === '/v1/blocks/stream') {
        const from = intParam(url, 'from', Math.min(...x.blocks.keys()), 0, Number.MAX_SAFE_INTEGER);
        res.writeHead(200, { ...headers, 'content-type': 'application/x-ndjson' });
        for (let h = Math.max(from, Math.min(...x.blocks.keys())); h <= x.height; h++) res.write(JSON.stringify(W.block(x, x.blocks.get(h))) + '\n');
        return res.end();
      }
      for (const [re, fn] of routes) {
        const m = re.exec(path);
        if (m) return send(res, 200, fn(url, m.slice(1), req));
      }
      throw new ApiError(404, 'NOT_FOUND', 'no such endpoint');
    } catch (e) {
      if (e instanceof ApiError) return send(res, e.http, err(e), e.http === 429 ? { 'retry-after': String(e.details.retryAfter ?? 1) } : {});
      if (e instanceof EngineError) return send(res, 400, err({ code: 'MALFORMED', message: e.message }));
      return send(res, 500, err({ code: 'INTERNAL', message: String(e?.message ?? e).slice(0, 200) }));
    }
  });

  // ---------- WebSocket (RFC 6455; text frames; server never masks) ----------
  const perIp = new Map();
  server.on('upgrade', (req, socket) => {
    const ip = ipOf(req);
    if (!req.url.startsWith('/v1/ws') || state.refuseSockets || req.headers.upgrade?.toLowerCase() !== 'websocket' || !req.headers['sec-websocket-key'] || (perIp.get(ip) ?? 0) >= LIMITS.wsPerIp) {
      socket.end('HTTP/1.1 400 Bad Request\r\n\r\n');
      return;
    }
    const accept = createHash('sha1').update(req.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    perIp.set(ip, (perIp.get(ip) ?? 0) + 1);
    const conn = { socket, subs: new Map(), buf: Buffer.alloc(0), last: Date.now(), ip, frag: null, country: countryOf(req, new URL(req.url, 'http://127.0.0.1')) };
    sockets.add(conn);
    socket.setNoDelay(true);
    socket.on('data', (d) => onData(conn, d));
    const bye = () => {
      if (!sockets.delete(conn)) return;
      perIp.set(ip, perIp.get(ip) - 1);
    };
    socket.on('close', bye);
    socket.on('error', bye);
  });
  const pinger = setInterval(() => {
    const now = Date.now();
    for (const c of sockets) {
      if (now - c.last > 60_000) closeConn(c, 1001, 'idle');
      else rawFrame(c, 0x9, Buffer.alloc(0));
    }
  }, 15_000);
  pinger.unref();

  function rawFrame(c, op, payload) {
    const n = payload.length;
    const head = n < 126 ? Buffer.from([0x80 | op, n]) : n < 65536 ? Buffer.from([0x80 | op, 126, n >> 8, n & 255]) : Buffer.concat([Buffer.from([0x80 | op, 127]), (() => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(n)); return b; })()]);
    if (c.socket.writableLength > 0 && c.socket.writableNeedDrain) {
      c.queued = (c.queued ?? 0) + 1;
      if (c.queued > LIMITS.wsOutQueue) {
        c.socket.destroy();
        return;
      }
      c.socket.once('drain', () => (c.queued = 0));
    }
    c.socket.write(Buffer.concat([head, payload]));
  }
  const sendWs = (c, obj) => rawFrame(c, 0x1, Buffer.from(JSON.stringify(obj)));
  function closeConn(c, code, reason) {
    const p = Buffer.alloc(2 + Buffer.byteLength(reason));
    p.writeUInt16BE(code);
    p.write(reason, 2);
    rawFrame(c, 0x8, p);
    c.socket.end();
  }
  function onData(c, d) {
    c.last = Date.now();
    c.buf = Buffer.concat([c.buf, d]);
    for (;;) {
      if (c.buf.length < 2) return;
      const b0 = c.buf[0], b1 = c.buf[1];
      const fin = (b0 & 0x80) !== 0, op = b0 & 0x0f, masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f, off = 2;
      if (len === 126) { if (c.buf.length < 4) return; len = c.buf.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (c.buf.length < 10) return; const big = c.buf.readBigUInt64BE(2); if (big > BigInt(LIMITS.wsIn)) return closeConn(c, 1009, 'TOO_LARGE'); len = Number(big); off = 10; }
      if (!masked) return closeConn(c, 1002, 'client frames must be masked');
      if (len > LIMITS.wsIn) return closeConn(c, 1009, 'TOO_LARGE');
      if (c.buf.length < off + 4 + len) return;
      const mask = c.buf.subarray(off, off + 4);
      const payload = Buffer.from(c.buf.subarray(off + 4, off + 4 + len));
      for (let i = 0; i < len; i++) payload[i] ^= mask[i & 3];
      c.buf = c.buf.subarray(off + 4 + len);
      if (op === 0x8) return closeConn(c, 1000, 'bye');
      if (op === 0x9) { rawFrame(c, 0xa, payload); continue; }
      if (op === 0xa) continue;
      if (op === 0x1 || op === 0x0) {
        c.frag = op === 0x1 ? payload : Buffer.concat([c.frag ?? Buffer.alloc(0), payload]);
        if (c.frag.length > LIMITS.wsIn) return closeConn(c, 1009, 'TOO_LARGE');
        if (!fin) continue;
        const text = c.frag.toString('utf8');
        c.frag = null;
        onMessage(c, text);
        continue;
      }
      return closeConn(c, 1003, 'unsupported frame');
    }
  }
  const subKey = (s) => `${s.channel}|${s.symbol ?? ''}|${s.interval ?? ''}|${s.account ?? ''}`;
  function onMessage(c, text) {
    let msg;
    try {
      msg = JSON.parse(text);
    } catch {
      return sendWs(c, { op: 'error', error: { code: 'MALFORMED', message: 'not JSON', details: {} } });
    }
    const id = msg?.id;
    const wsErr = (e) => sendWs(c, { op: 'error', ...(id !== undefined ? { id } : {}), error: { code: e.code, message: e.message, details: e.details ?? {} } });
    try {
      if (msg.op === 'ping') return sendWs(c, { op: 'pong', time: x.time });
      if (msg.op === 'action') {
        const { ack } = admitAction(msg.request, c.country, c.ip);
        return sendWs(c, { op: 'actionResult', ...(id !== undefined ? { id } : {}), ack });
      }
      if (msg.op !== 'subscribe' && msg.op !== 'unsubscribe') throw new ApiError(400, 'MALFORMED', 'op is subscribe, unsubscribe, action or ping', { path: 'op' });
      const s = { channel: msg.channel };
      if (!['book', 'trades', 'ticker', 'candles', 'account', 'status'].includes(msg.channel)) throw new ApiError(400, 'MALFORMED', 'unknown channel', { path: 'channel' });
      if (['book', 'trades', 'candles'].includes(msg.channel) || (msg.channel === 'ticker' && msg.symbol !== '*')) {
        marketOf(msg.symbol);
        s.symbol = msg.symbol;
      }
      if (msg.channel === 'ticker' && msg.symbol === '*') s.symbol = '*';
      if (msg.channel === 'candles') {
        if (!INTERVALS[msg.interval]) throw new ApiError(400, 'MALFORMED', 'unknown interval', { path: 'interval' });
        s.interval = msg.interval;
      }
      if (msg.channel === 'account') {
        if (typeof msg.account !== 'string' || !/^0x[0-9a-fA-F]{40}$/.test(msg.account)) throw new ApiError(400, 'MALFORMED', 'account must be an address', { path: 'account' });
        s.account = msg.account.toLowerCase();
      }
      if (msg.channel === 'book') s.depth = msg.depth === undefined ? 50 : Number(msg.depth);
      if (msg.channel === 'book' && (!Number.isInteger(s.depth) || s.depth < 1 || s.depth > 500)) throw new ApiError(400, 'MALFORMED', 'depth 1 to 500', { path: 'depth' });
      const k = subKey(s);
      const echo = { channel: s.channel, ...(s.symbol ? { symbol: s.symbol } : {}), ...(s.interval ? { interval: s.interval } : {}), ...(s.account ? { account: s.account } : {}) };
      if (msg.op === 'unsubscribe') {
        c.subs.delete(k);
        return sendWs(c, { op: 'unsubscribed', ...(id !== undefined ? { id } : {}), ...echo });
      }
      if (!c.subs.has(k) && c.subs.size >= LIMITS.subsPerConn) throw new ApiError(429, 'RATE_LIMITED', 'too many subscriptions');
      c.subs.set(k, { ...s, tickerAt: new Map() });
      sendWs(c, { op: 'subscribed', ...(id !== undefined ? { id } : {}), ...echo });
      snapshot(c, c.subs.get(k));
    } catch (e) {
      if (e instanceof ApiError) return wsErr(e);
      return wsErr({ code: 'INTERNAL', message: String(e?.message ?? e).slice(0, 200) });
    }
  }
  function snapshot(c, s) {
    const m = s.symbol && s.symbol !== '*' ? x.markets.get(s.symbol) : null;
    if (s.channel === 'book') sendWs(c, { channel: 'book', symbol: s.symbol, type: 'snapshot', data: W.book(x, m, s.depth) });
    else if (s.channel === 'trades') sendWs(c, { channel: 'trades', symbol: s.symbol, type: 'snapshot', data: { trades: m.trades.slice(0, 50).map(W.trade) } });
    else if (s.channel === 'candles') sendWs(c, { channel: 'candles', symbol: s.symbol, interval: s.interval, type: 'snapshot', data: { candles: m.candles[s.interval].slice(-300).map((k) => W.candle(m, k)) } });
    else if (s.channel === 'ticker') for (const mm of m ? [m] : x.markets.values()) { s.tickerAt.set(mm, Date.now()); sendWs(c, { channel: 'ticker', symbol: mm.symbol, type: 'update', data: W.ticker(x, mm) }); }
    else if (s.channel === 'account') {
      const A = x.accountForRead(s.account);
      sendWs(c, { channel: 'account', account: A.address, type: 'snapshot', data: { account: W.account(x, A), orders: [...A.orders.values()].map(W.order), agents: [...A.agents.values()].filter((g) => g.expiresAt > x.time).map(W.agent) } });
    } else if (s.channel === 'status') sendWs(c, { channel: 'status', type: 'update', data: statusUpdate() });
  }
  const statusUpdate = () => {
    const st = W.status(x, 'mock');
    return { latestBlock: st.latestBlock, latestFinalized: st.latestFinalized };
  };
  function broadcastStatus() {
    for (const c of sockets) for (const s of c.subs.values()) if (s.channel === 'status') sendWs(c, { channel: 'status', type: 'update', data: statusUpdate() });
  }
  function broadcast(ev) {
    if (ev.block) for (const w of waiters.splice(0)) w();
    if (!sockets.size) return;
    const now = Date.now();
    const accUpdates = new Map();
    for (const [addr, e] of ev.accounts) {
      const A = x.account(addr);
      const u = { height: x.height, time: x.time };
      if (e.account || e.fills.length || e.orders.size || e.funding.length) u.account = W.account(x, A);
      if (e.orders.size) u.orders = [...e.orders.values()].map(W.order);
      if (e.fills.length) u.fills = e.fills.map(W.fill);
      if (e.funding.length) u.funding = e.funding.map(W.fundingPayment);
      if (e.deposits.size) u.deposits = [...e.deposits.values()].map(W.deposit);
      if (e.withdrawals.size) u.withdrawals = [...e.withdrawals.values()].map((w) => W.withdrawal(x, w));
      if (e.actions.length) u.actions = e.actions;
      if (Object.keys(u).length > 2) accUpdates.set(addr, u);
    }
    for (const c of sockets) {
      for (const s of c.subs.values()) {
        const m = s.symbol && s.symbol !== '*' ? x.markets.get(s.symbol) : null;
        if (s.channel === 'book' && ev.deltas.has(m)) sendWs(c, { channel: 'book', symbol: m.symbol, type: 'delta', data: W.bookDelta(x, m, ev.deltas.get(m)) });
        else if (s.channel === 'trades' && ev.trades.has(m)) sendWs(c, { channel: 'trades', symbol: m.symbol, type: 'update', data: { trades: ev.trades.get(m).map(W.trade) } });
        else if (s.channel === 'candles') {
          for (const k of ev.candles.get(`${m.symbol}|${s.interval}`) ?? []) sendWs(c, { channel: 'candles', symbol: m.symbol, interval: s.interval, type: 'update', data: W.candle(m, k) });
        } else if (s.channel === 'ticker') {
          for (const mm of m ? [m] : x.markets.values()) {
            if (ev.tickers.has(mm) || now - (s.tickerAt.get(mm) ?? 0) >= 1000) {
              s.tickerAt.set(mm, now);
              sendWs(c, { channel: 'ticker', symbol: mm.symbol, type: 'update', data: W.ticker(x, mm) });
            }
          }
        } else if (s.channel === 'account' && accUpdates.has(s.account)) sendWs(c, { channel: 'account', account: s.account, type: 'update', data: accUpdates.get(s.account) });
        else if (s.channel === 'status' && (ev.block || ev.checkpoint)) sendWs(c, { channel: 'status', type: 'update', data: statusUpdate() });
      }
    }
  }

  const host = '127.0.0.1';
  await new Promise((r) => server.listen(opts.port ?? 49300, host, r));
  const port = server.address().port;
  return {
    x, server, port, url: `http://${host}:${port}`, step, state,
    close: () => new Promise((r) => {
      clearInterval(timer);
      clearInterval(pinger);
      for (const c of sockets) c.socket.destroy();
      server.close(() => r());
    }),
  };
}

// ---------- CLI ----------
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const opt = (name, fallback) => {
    const i = args.indexOf(`--${name}`);
    return i >= 0 ? args[i + 1] : fallback;
  };
  for (const a of args) if (a.startsWith('--') && !['--port', '--seed', '--start-time', '--static', '--blocks', '--block-ms', '--no-rate-limit', '--nonce-window', '--eip712', '--markets', '--host'].includes(a)) throw new Error(`unknown option ${a}`);
  if (opt('host', '127.0.0.1') !== '127.0.0.1') throw new Error('the mock listens on 127.0.0.1 only');
  const nw = opt('nonce-window', null);
  const m = await startMock({
    port: Number(opt('port', '49300')), seed: Number(opt('seed', '7')), startTime: Number(opt('start-time', '1794878400000')),
    static: args.includes('--static'), blocks: Number(opt('blocks', '40')), blockMs: Number(opt('block-ms', '200')),
    rateLimit: !args.includes('--no-rate-limit'), nonceWindowMs: nw ? nw.split(':').map(Number) : null,
    eip712File: opt('eip712', undefined), marketsFile: opt('markets', undefined),
  });
  process.stdout.write(`postern-perps mock (UNAUDITED) on ${m.url}  pid ${process.pid}  seed ${m.x.seed}  demo ${m.x.demo}\n`);
  const stop = () => m.close().then(() => process.exit(0));
  process.on('SIGINT', stop);
  process.on('SIGTERM', stop);
}
