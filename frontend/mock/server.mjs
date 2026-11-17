#!/usr/bin/env node
// Frontend stand-in of the PosternDEX Perpetuals API (docs/API.md v0.1). UNAUDITED prototype, development only.
//
// Agent 393 owns the reference mock (`node/mock/`); this file exists so the frontend's screens, tests and browser
// harness have a deterministic exchange before that lands, and it stays as the harness's scenario driver. The harness
// takes the mock by path (POSTERN_MOCK), so it runs against 393's mock as well.
//
//   node mock/server.mjs [--port 49110] [--seed 7] [--start-time 1794878400000] [--static] [--blocks 40]
//
// --static stops the clock after the initial blocks (screenshots are byte-stable); otherwise a block is produced
// every 200 ms. Scenario switches: GET /mock/scenario?<name>=<value> (see SCENARIOS below). Loopback only. Node 24
// built-ins plus viem (already a frontend dependency) for real EIP-712 signature recovery.
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { recoverTypedDataAddress } from 'viem';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const PORT = Number(opt('port', '49110'));
const SEED = Number(opt('seed', '7'));
const START = Number(opt('start-time', '1794878400000')); // 2026-11-17 is near this engine time
const STATIC = args.includes('--static');
const INITIAL_BLOCKS = Number(opt('blocks', '40'));
const BLOCK_MS = 200;

// ---------- Deterministic randomness ----------
function prng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rand = prng(SEED);
const gauss = (r = rand) => {
  let u = 0;
  while (u === 0) u = r();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * r());
};

// ---------- Fixtures ----------
const PINNED = JSON.parse(readFileSync(new URL('../config/markets.json', import.meta.url), 'utf8'));
const TYPES = JSON.parse(readFileSync(new URL('./eip712.json', import.meta.url), 'utf8'));
const SIM = {
  'BTC-USD': { start: 67250, vol: 0.00018, level: 0.8, oiCap: '1000', vol24: 18_233_412.55, oi: 412.3312 },
  'ETH-USD': { start: 3480, vol: 0.00024, level: 12, oiCap: '20000', vol24: 9_812_400.1, oi: 6120.442 },
  'SOL-USD': { start: 182.4, vol: 0.00034, level: 220, oiCap: '400000', vol24: 2_411_905.72, oi: 88412.5 },
};
const DEFAULTS = { makerFeeBps: '1.5', takerFeeBps: '4.5', liquidationFeeBps: '50', fundingIntervalSec: 3600, maxFundingRate: '0.0005' };
const ORACLE = { sources: ['binance', 'coinbase', 'kraken', 'bitstamp', 'okx'], minSources: 3, maxStalenessMs: 10000, maxDeviationBps: '200' };
const WITHDRAWAL_FEE = 1; // USDC, covers the L2 gas (BRIEF-PERPS fee table)

const fx = (v, dp) => (Number.isFinite(v) ? v.toFixed(dp) : '0');
const round = (v, step) => Math.round(v / step) * step;
const hex = (bytes) => `0x${Buffer.from(bytes).toString('hex')}`;
const h32 = (text) => hex(createHash('sha256').update(String(text)).digest());

// ---------- Exchange state ----------
const state = {
  height: 120000,
  time: START,
  finalized: 119996,
  finalityStalled: false,
  sequencer: 'running',
  vaultMode: 'normal',
  geoDefault: 'PA',
  sanctioned: new Set(['0x000000000000000000000000000000000000dead']),
  eip712: 'pinned',
  freshAccounts: false,
  busy: false,
  refuseSockets: 0,
  nextId: 9_912_100,
  tradeId: 771_300,
};
const markets = new Map();
for (const m of PINNED.markets) {
  const sim = SIM[m.symbol];
  const tick = Number(m.tickSize);
  markets.set(m.symbol, {
    pinned: m,
    tick,
    step: Number(m.stepSize),
    pd: m.priceDecimals,
    sd: m.sizeDecimals,
    status: 'active',
    haltReason: null,
    index: sim.start,
    mark: sim.start,
    last: sim.start,
    open24: sim.start * (1 - 0.006744),
    high24: sim.start * 1.0113,
    low24: sim.start * 0.9832,
    vol24: sim.vol24,
    oi: sim.oi,
    sim,
    bids: new Map(),
    asks: new Map(),
    seq: 88_000 + m.marketId * 1000,
    trades: [],
    candles: new Map(),
    fundingRate: 0.0000125,
  });
}
const nextFundingTime = () => Math.ceil((state.time + 1) / 3_600_000) * 3_600_000;

function ladder(m, side, mid) {
  const levels = new Map();
  const r = prng(SEED * 31 + m.pinned.marketId * (side === 'bid' ? 7 : 11) + Math.floor(state.height / 5));
  for (let i = 0; i < 60; i++) {
    const offset = (i + 1) * m.tick * (1 + Math.floor(r() * (i < 6 ? 1.5 : 4)));
    const px = round(side === 'bid' ? mid - offset : mid + offset, m.tick);
    const size = round(m.sim.level * (0.15 + r() * 1.6) * (1 + i / 12), m.step);
    if (size > 0) levels.set(fx(px, m.pd), fx(size, m.sd));
  }
  return levels;
}
function rebuildBook(m) {
  const mid = round(m.mark, m.tick);
  m.bids = ladder(m, 'bid', mid);
  m.asks = ladder(m, 'ask', mid + m.tick);
}
const sortedLevels = (map, desc) =>
  [...map.entries()].sort((a, b) => (desc ? Number(b[0]) - Number(a[0]) : Number(a[0]) - Number(b[0])));

// Candle history: a deterministic walk ending at the start price, per interval.
const INTERVALS = { '1m': 60_000, '5m': 300_000, '15m': 900_000, '1h': 3_600_000, '4h': 14_400_000, '1d': 86_400_000 };
function history(m, interval) {
  if (m.candles.has(interval)) return m.candles.get(interval);
  const ms = INTERVALS[interval];
  const r = prng(SEED * 97 + m.pinned.marketId * 13 + ms / 60_000);
  const n = 1500;
  const out = [];
  let close = m.sim.start;
  const sigma = m.sim.vol * Math.sqrt(ms / BLOCK_MS) * 0.35;
  const end = Math.floor(START / ms) * ms;
  for (let i = 0; i < n; i++) {
    const t = end - i * ms;
    const drift = Math.sin(i / 37) * sigma * 0.4;
    const open = close * (1 - (gauss(r) * sigma + drift));
    const hi = Math.max(open, close) * (1 + Math.abs(gauss(r)) * sigma * 0.6);
    const lo = Math.min(open, close) * (1 - Math.abs(gauss(r)) * sigma * 0.6);
    const v = m.sim.level * (2 + r() * 18) * Math.sqrt(ms / 60_000);
    out.push({ t, o: open, h: hi, l: lo, c: close, v, n: 20 + Math.floor(r() * 400) });
    close = open;
  }
  out.reverse();
  m.candles.set(interval, out);
  return out;
}
const wireCandle = (m, c) => ({ t: c.t, o: fx(c.o, m.pd), h: fx(c.h, m.pd), l: fx(c.l, m.pd), c: fx(c.c, m.pd), v: fx(c.v, m.sd), q: fx(c.v * c.c, 2), n: c.n });

// ---------- Accounts ----------
const accounts = new Map();
const ZERO = '0x0000000000000000000000000000000000000000';
function demoAccount(address) {
  const t = START;
  const a = {
    address,
    collateral: 25_000,
    positions: [
      { symbol: 'BTC-USD', size: 0.5, entry: 66_800, leverage: 10, isolated: false, isolatedMargin: 0, realized: 0, funding: -1.22 },
      { symbol: 'ETH-USD', size: -4.2, entry: 3512.4, leverage: 5, isolated: true, isolatedMargin: 2950, realized: 0, funding: 3.4 },
    ],
    orders: [
      order(address, 'BTC-USD', 'buy', 'limit', 'GTC', 66_000, 0.25, 0, { createdAt: t - 3_600_000 }),
      order(address, 'BTC-USD', 'sell', 'stopMarket', 'GTC', 0, 0.5, 0, { trigger: 63_900, reduceOnly: true, createdAt: t - 7_200_000 }),
      order(address, 'ETH-USD', 'buy', 'takeProfitMarket', 'GTC', 0, 4.2, 0, { trigger: 3_310, reduceOnly: true, createdAt: t - 5_400_000 }),
      order(address, 'SOL-USD', 'buy', 'limit', 'POST_ONLY', 176.5, 40, 12.5, { createdAt: t - 900_000 }),
    ],
    history: [],
    fills: [],
    funding: [],
    deposits: [
      { depositId: '41', amount: '20000.000000', l2TxHash: h32('dep41'), logIndex: 3, l2Block: 18_120, status: 'credited', creditedHeight: 118_200, time: t - 86_400_000 * 3 },
      { depositId: '44', amount: '5000.000000', l2TxHash: h32('dep44'), logIndex: 1, l2Block: 18_230, status: 'credited', creditedHeight: 119_800, time: t - 86_400_000 },
      { depositId: '47', amount: '1500.000000', l2TxHash: h32('dep47'), logIndex: 0, l2Block: 18_241, status: 'l2Finalized', creditedHeight: null, time: t - 240_000 },
      { depositId: '48', amount: '250.000000', l2TxHash: h32('dep48'), logIndex: 0, l2Block: 18_244, status: 'seen', creditedHeight: null, time: t - 60_000 },
    ],
    withdrawals: [
      withdrawal(address, '71', 1200, 'paid', t - 86_400_000 * 2, { l2TxHash: h32('w71') }),
      withdrawal(address, '76', 40_000, 'delayed', t - 3_600_000 * 5, { availableAt: t + 3_600_000 * 19 }),
      withdrawal(address, '77', 1000, 'certified', t - 1_200_000, {}),
      withdrawal(address, '78', 300, 'requested', t - 30_000, {}),
    ],
    agents: [{ agent: '0x4b1a6c2f9e0d7a35b8c1e2f3a4b5c6d7e8f90a1b', name: 'desk-bot', approvedAt: t - 86_400_000 * 6, expiresAt: t + 86_400_000 * 24, height: 98_000 }],
    nonce: 0,
    volume30d: 1_284_550.12,
  };
  // Fills and funding history, newest first.
  const r = prng(SEED * 5 + 3);
  for (let i = 0; i < 36; i++) {
    const sym = ['BTC-USD', 'ETH-USD', 'SOL-USD'][i % 3];
    const m = markets.get(sym);
    const px = m.sim.start * (1 + gauss(r) * 0.01);
    const size = round(m.sim.level * (0.05 + r() * 0.6), m.step);
    const buy = r() > 0.45;
    const maker = r() > 0.5;
    const fee = px * size * (maker ? 0.00015 : 0.00045);
    a.fills.push({
      fillId: String(5_512_000 - i), tradeId: String(771_200 - i), orderId: String(9_912_000 - i), account: address, symbol: sym,
      side: buy ? 'buy' : 'sell', price: fx(round(px, m.tick), m.pd), size: fx(size, m.sd), fee: fx(fee, 6),
      liquidity: maker ? 'maker' : 'taker', realizedPnl: fx(i % 4 === 0 ? gauss(r) * 120 : 0, 6), liquidation: false,
      time: t - (i + 1) * 2_350_000, height: 119_000 - i * 40,
    });
  }
  for (let i = 0; i < 24; i++) {
    const rate = 0.0000125 + gauss(r) * 0.000006;
    a.funding.push({ symbol: 'BTC-USD', time: Math.floor(t / 3_600_000) * 3_600_000 - i * 3_600_000, rate: fx(rate, 7), positionSize: '0.5000', markPrice: fx(66_900 + gauss(r) * 300, 1), payment: fx(-0.5 * 66_900 * rate, 6) });
    a.funding.push({ symbol: 'ETH-USD', time: Math.floor(t / 3_600_000) * 3_600_000 - i * 3_600_000, rate: fx(rate * 0.8, 7), positionSize: '-4.200', markPrice: fx(3_490 + gauss(r) * 20, 2), payment: fx(4.2 * 3_490 * rate * 0.8, 6) });
  }
  a.history = [
    order(address, 'BTC-USD', 'buy', 'limit', 'GTC', 66_800, 0.5, 0.5, { status: 'filled', createdAt: t - 86_400_000 }),
    order(address, 'ETH-USD', 'sell', 'market', 'IOC', 3_480, 4.2, 4.2, { status: 'filled', createdAt: t - 80_000_000 }),
    order(address, 'SOL-USD', 'buy', 'limit', 'GTC', 170, 50, 0, { status: 'canceled', createdAt: t - 70_000_000 }),
    order(address, 'BTC-USD', 'sell', 'limit', 'POST_ONLY', 67_000, 0.1, 0, { status: 'rejected', rejectReason: 'POST_ONLY_WOULD_CROSS', createdAt: t - 60_000_000 }),
  ];
  return a;
}
function order(account, symbol, side, type, tif, price, size, filled, o = {}) {
  const m = markets.get(symbol);
  const id = String(state.nextId++);
  const trigger = o.trigger ?? null;
  return {
    orderId: id, clientOrderId: null, account, symbol, side, type, timeInForce: tif,
    price: price ? fx(price, m.pd) : null, size: fx(size, m.sd), filledSize: fx(filled, m.sd), avgFillPrice: filled ? fx(price, m.pd) : null,
    reduceOnly: !!o.reduceOnly, triggerPrice: trigger ? fx(trigger, m.pd) : null,
    triggerCondition: trigger ? ((type.startsWith('stop') ? side === 'buy' : side !== 'buy') ? 'markAbove' : 'markBelow') : null,
    status: o.status ?? (trigger ? 'untriggered' : 'open'), createdAt: o.createdAt ?? state.time, updatedAt: o.createdAt ?? state.time,
    height: state.height - 100, rejectReason: o.rejectReason ?? null,
  };
}
function withdrawal(account, id, amount, status, time, o) {
  const certified = ['certified', 'submitted', 'delayed', 'paid'].includes(status);
  return {
    withdrawalId: id, account, recipient: account, amount: fx(amount, 6), fee: fx(WITHDRAWAL_FEE, 6), status,
    requestedHeight: 119_000, checkpointHeight: certified ? 119_040 : null,
    certificate: certified ? { digest: h32(`wd${id}`), record: { withdrawalId: id, account, recipient: account, amount: fx(amount, 6) }, checkpoint: null, proof: [h32(`p${id}`)], signatures: [] } : null,
    l2TxHash: o.l2TxHash ?? null, availableAt: o.availableAt ?? null, time,
  };
}
function getAccount(address) {
  const key = address.toLowerCase();
  if (!accounts.has(key)) accounts.set(key, state.freshAccounts ? { ...demoAccount(key), collateral: 0, positions: [], orders: [], history: [], fills: [], funding: [], deposits: [], withdrawals: [], agents: [] } : demoAccount(key));
  return accounts.get(key);
}
function tierOf(m, notional) {
  let tier = m.pinned.marginTiers[0];
  for (const t of m.pinned.marginTiers) if (notional >= Number(t.notionalFloor)) tier = t;
  return tier;
}
function wireAccount(a) {
  let upnl = 0;
  let im = 0;
  let mm = 0;
  let isolated = 0;
  const positions = a.positions.map((p) => {
    const m = markets.get(p.symbol);
    const notional = Math.abs(p.size) * m.mark;
    const tier = tierOf(m, notional);
    const u = p.size * (m.mark - p.entry);
    const pim = p.isolated ? p.isolatedMargin : notional / p.leverage;
    const pmm = notional * Number(tier.maintenanceMarginFraction);
    upnl += u;
    im += pim;
    mm += pmm;
    if (p.isolated) isolated += p.isolatedMargin;
    return { p, m, notional, u, pim, pmm, tier };
  });
  const equity = a.collateral + upnl;
  const crossBacking = a.collateral - isolated + positions.filter((x) => !x.p.isolated).reduce((s, x) => s + x.u, 0);
  const wirePositions = positions.map(({ p, m, notional, u, pim, pmm, tier }) => {
    const mmr = Number(tier.maintenanceMarginFraction);
    const q = Math.abs(p.size);
    const backing = p.isolated ? p.isolatedMargin : crossBacking - u - positions.filter((x) => x.p !== p && !x.p.isolated).reduce((s, x) => s + x.pmm, 0);
    const liq = p.size > 0 ? (q * p.entry - backing) / (q * (1 - mmr)) : (q * p.entry + backing) / (q * (1 + mmr));
    return {
      symbol: p.symbol, size: fx(p.size, m.sd), entryPrice: fx(p.entry, m.pd), markPrice: fx(m.mark, m.pd), notional: fx(notional, 6),
      unrealizedPnl: fx(u, 6), realizedPnl: fx(p.realized, 6), liquidationPrice: liq > 0 ? fx(round(liq, m.tick), m.pd) : null,
      marginMode: p.isolated ? 'isolated' : 'cross', leverage: String(p.leverage), isolatedMargin: p.isolated ? fx(p.isolatedMargin, 6) : null,
      initialMargin: fx(pim, 6), maintenanceMargin: fx(pmm, 6), fundingSinceOpen: fx(p.funding, 6),
    };
  });
  const orderMargin = a.orders.filter((o) => o.status === 'open' && !o.reduceOnly).reduce((s, o) => s + (Number(o.size) - Number(o.filledSize)) * Number(o.price ?? 0) / 10, 0);
  const free = Math.max(0, equity - im - orderMargin);
  return {
    address: a.address, height: state.height, time: state.time, collateral: fx(a.collateral, 6), unrealizedPnl: fx(upnl, 6), equity: fx(equity, 6),
    initialMargin: fx(im + orderMargin, 6), maintenanceMargin: fx(mm, 6), freeCollateral: fx(free, 6), withdrawable: fx(Math.max(0, free - 0), 6),
    marginRatio: fx(equity > 0 ? mm / equity : 1, 6),
    feeTier: { tier: a.volume30d > 5_000_000 ? 1 : 0, makerFeeBps: '1.5', takerFeeBps: '4.5', volume30d: fx(a.volume30d, 2) },
    positions: wirePositions, openOrders: a.orders.length,
  };
}

// ---------- Simulation ----------
function step() {
  state.height++;
  state.time += BLOCK_MS;
  if (!state.finalityStalled && state.height % 5 === 0) state.finalized = state.height - 4;
  const blockTrades = new Map();
  const deltas = new Map();
  for (const m of markets.values()) {
    if (m.status === 'halted') continue;
    const before = { bids: m.bids, asks: m.asks };
    m.index *= 1 + gauss() * m.sim.vol;
    const premium = Math.max(-0.0005, Math.min(0.0005, gauss() * 0.00008));
    m.mark = m.index * (1 + premium);
    rebuildBook(m);
    // Takers.
    const n = state.busy ? 6 : rand() < 0.55 ? 1 + Math.floor(rand() * 3) : 0;
    const trades = [];
    for (let i = 0; i < n; i++) {
      const buy = rand() > 0.5;
      const best = sortedLevels(buy ? m.asks : m.bids, !buy)[0];
      if (!best) continue;
      const size = round(m.sim.level * (0.01 + rand() * 0.3), m.step) || m.step;
      const t = { tradeId: String(state.tradeId++), symbol: m.pinned.symbol, price: best[0], size: fx(size, m.sd), side: buy ? 'buy' : 'sell', time: state.time, height: state.height, liquidation: false };
      trades.push(t);
      m.last = Number(best[0]);
      m.vol24 += size * m.last;
    }
    m.high24 = Math.max(m.high24, m.last);
    m.low24 = Math.min(m.low24, m.last);
    m.trades = [...trades.slice().reverse(), ...m.trades].slice(0, 200);
    blockTrades.set(m.pinned.symbol, trades);
    // Book delta: every changed level.
    const d = { bids: [], asks: [] };
    for (const [side, now, was] of [['bids', m.bids, before.bids], ['asks', m.asks, before.asks]]) {
      for (const [px, size] of now) if (was.get(px) !== size) d[side].push([px, size]);
      for (const px of was.keys()) if (!now.has(px)) d[side].push([px, '0']);
    }
    m.seq++;
    deltas.set(m.pinned.symbol, { symbol: m.pinned.symbol, seq: m.seq, prevSeq: m.seq - 1, height: state.height, time: state.time, ...d });
    // Candles: the open candle of every interval follows the last price.
    for (const [interval, ms] of Object.entries(INTERVALS)) {
      const list = history(m, interval);
      const open = Math.floor(state.time / ms) * ms;
      let c = list[list.length - 1];
      if (c.t < open) {
        c = { t: open, o: c.c, h: c.c, l: c.c, c: c.c, v: 0, n: 0 };
        list.push(c);
      }
      c.c = m.last;
      c.h = Math.max(c.h, m.last);
      c.l = Math.min(c.l, m.last);
      for (const t of trades) {
        c.v += Number(t.size);
        c.n++;
      }
    }
    m.fundingRate = Math.max(-0.0005, Math.min(0.0005, 0.0000125 + premium * 0.12));
  }
  return { blockTrades, deltas };
}
function rebuildAll() {
  for (const m of markets.values()) rebuildBook(m);
}
rebuildAll();
for (let i = 0; i < INITIAL_BLOCKS; i++) step();

const wireMarket = (m) => ({
  ...m.pinned,
  quote: 'USD',
  status: m.status,
  maxOrderSize: m.pinned.maxOrderSize,
  ...DEFAULTS,
  oracle: ORACLE,
  openInterestCap: m.sim.oiCap,
  haltReason: m.haltReason,
});
const wireTicker = (m) => ({
  symbol: m.pinned.symbol, markPrice: fx(m.mark, m.pd), indexPrice: fx(m.index, m.pd), lastPrice: fx(m.last, m.pd),
  fundingRate: fx(m.fundingRate, 7), nextFundingTime: nextFundingTime(), openInterest: fx(m.oi, m.sd), volume24h: fx(m.vol24, 2),
  high24h: fx(m.high24, m.pd), low24h: fx(m.low24, m.pd), price24hAgo: fx(m.open24, m.pd), change24h: fx(m.last / m.open24 - 1, 6),
  height: state.height, time: state.time,
});
const wireBook = (m, depth = 50) => ({
  symbol: m.pinned.symbol, seq: m.seq, height: state.height, time: state.time,
  bids: sortedLevels(m.bids, true).slice(0, depth), asks: sortedLevels(m.asks, false).slice(0, depth),
});
const blockRef = (height) => ({ height, time: START + (height - 120000) * BLOCK_MS, hash: h32(`b${height}`), stateRoot: h32(`s${height}`), logHash: h32(`l${height}`) });
const status = () => ({
  name: 'postern-perps-node', version: '0.1.0', apiVersion: '0.1', mode: 'mock', unaudited: true,
  network: { eip712: TYPES.domain, collateralChainId: 700770, vault: TYPES.domain.verifyingContract, collateralToken: ZERO, collateralSymbol: 'bUSDC', collateralDecimals: 6 },
  time: state.time, latestBlock: blockRef(state.height), latestFinalized: blockRef(state.finalized),
  sequencer: { state: state.sequencer, blockIntervalMs: BLOCK_MS, pendingActions: 3 }, committee: { n: 7, threshold: 5, online: state.finalityStalled ? 4 : 7 },
  vaultMode: state.vaultMode, l2: { finalizedBlock: 18_244, lastDepositBlock: 18_244, endpointsAgree: true },
});
const oracle = (m) => ({
  symbol: m.pinned.symbol, status: m.status === 'halted' ? 'halted' : 'ok', price: fx(m.index, m.pd), time: state.time - 300, round: String(553_120 + state.height),
  sources: ORACLE.sources.map((name, i) => ({ name, price: fx(m.index * (1 + (i - 2) * 0.00004), m.pd), time: state.time - 200 - i * 90, used: true })),
  signers: 5, threshold: 5, haltReason: m.haltReason,
});

// ---------- Geo ----------
function geo(req, url) {
  const country = (url.searchParams.get('country') ?? req.headers['x-mock-country'] ?? state.geoDefault ?? '').toString().toUpperCase() || null;
  const restricted = !country || !['PA', 'ZZ'].includes(country);
  return {
    country, restricted, policy: restricted ? 'exit_only' : 'open', listVersion: '0.1.1', reasonCode: restricted ? (country ? 'JURISDICTION' : 'UNKNOWN_COUNTRY') : null,
    message: restricted ? 'PosternDEX Perpetuals is not available in your jurisdiction.' : null, sanctioned: false,
  };
}

// ---------- Actions ----------
const EXIT = new Set(['CancelOrder', 'CancelAll', 'Withdraw']);
const acks = new Map();
async function action(req, url, body) {
  const g = geo(req, url);
  const { type, message, signature } = body ?? {};
  if (!type || !message || typeof signature !== 'string' || !TYPES.types[type]) return [400, err('MALFORMED', 'Unknown action or missing fields.')];
  let signer;
  try {
    signer = (await recoverTypedDataAddress({ domain: TYPES.domain, types: { [type]: TYPES.types[type] }, primaryType: type, message: coerce(type, message), signature })).toLowerCase();
  } catch {
    return [400, err('BAD_SIGNATURE', 'The signature does not recover.')];
  }
  const account = String(message.account).toLowerCase();
  if (state.sanctioned.has(account)) return [451, err('SANCTIONED', 'This address cannot use PosternDEX Perpetuals.')];
  const exitOnly = EXIT.has(type) || (type === 'PlaceOrder' && message.reduceOnly === true);
  if (g.restricted && !exitOnly) return [451, err('GEO_RESTRICTED', 'Not available in your jurisdiction.', { country: g.country, listVersion: g.listVersion })];
  if (state.sequencer !== 'running') return [503, err('SEQUENCER_UNAVAILABLE', 'The sequencer is recovering.')];
  const a = getAccount(account);
  const isAgent = signer !== account;
  if (isAgent && !a.agents.some((x) => x.agent === signer)) return [403, err('UNKNOWN_SIGNER', 'Signer is not approved for this account.')];
  if (isAgent && !TYPES.agentAllowed.includes(type)) return [403, err('AGENT_NOT_ALLOWED', 'This action needs the main key.')];
  const nonce = BigInt(message.nonce);
  if (nonce <= BigInt(a.nonce)) return [400, err('BAD_NONCE', 'Nonce not above the highest used.')];
  a.nonce = String(nonce);
  const result = apply(a, type, message);
  const ack = { actionHash: h32(`${type}${signature}`), status: 'included', receivedAt: state.time, height: state.height + 1, index: 0, result };
  acks.set(ack.actionHash, ack);
  pushAccount(a, { orders: result.changedOrders, withdrawals: result.withdrawals, fills: result.fills });
  delete result.changedOrders;
  delete result.withdrawals;
  delete result.fills;
  return [200, ack];
}
function coerce(type, message) {
  const out = {};
  for (const f of TYPES.types[type]) {
    const v = message[f.name];
    out[f.name] = /^u?int/.test(f.type) ? BigInt(v) : v;
  }
  return out;
}
const marketById = (id) => [...markets.values()].find((m) => m.pinned.marketId === Number(id));
const fromScaled = (v, decimals) => Number(BigInt(v)) / 10 ** decimals;
function apply(a, type, msg) {
  const PXS = TYPES.scales.price;
  const SZS = TYPES.scales.size;
  const changedOrders = [];
  const fills = [];
  const withdrawals = [];
  if (type === 'PlaceOrder') {
    const m = marketById(msg.marketId);
    if (!m) return { ok: false, error: 'MARKET_NOT_FOUND', changedOrders, fills, withdrawals };
    if (m.status === 'halted') return { ok: false, error: 'MARKET_HALTED', changedOrders, fills, withdrawals };
    const kinds = Object.keys(TYPES.enums.orderType);
    const kind = kinds[Number(msg.orderType)];
    const tif = Object.keys(TYPES.enums.timeInForce)[Number(msg.timeInForce)];
    const buy = msg.isBuy === true;
    const price = fromScaled(msg.price, PXS);
    const size = fromScaled(msg.size, SZS);
    const trigger = fromScaled(msg.triggerPrice, PXS);
    const best = sortedLevels(buy ? m.asks : m.bids, !buy)[0];
    const crosses = best && (buy ? price >= Number(best[0]) : price <= Number(best[0]));
    if (tif === 'POST_ONLY' && crosses) return { ok: false, error: 'POST_ONLY_WOULD_CROSS', changedOrders, fills, withdrawals };
    const o = order(a.address, m.pinned.symbol, buy ? 'buy' : 'sell', kind, tif, price, size, 0, { trigger: trigger || null, reduceOnly: msg.reduceOnly === true });
    if ((kind === 'market' || tif === 'IOC' || crosses) && !trigger) {
      // Fill at the best level within the limit (simplified: one level, whole size).
      if (!crosses) return { ok: false, error: 'NO_LIQUIDITY', changedOrders, fills, withdrawals };
      const px = Number(best[0]);
      o.status = 'filled';
      o.filledSize = o.size;
      o.avgFillPrice = fx(px, m.pd);
      fill(a, m, buy, size, px, fills);
      changedOrders.push(o);
      return { ok: true, orderId: o.orderId, orderStatus: 'filled', filledSize: o.size, error: null, changedOrders, fills, withdrawals };
    }
    a.orders.unshift(o);
    changedOrders.push(o);
    return { ok: true, orderId: o.orderId, orderStatus: o.status, filledSize: '0', error: null, changedOrders, fills, withdrawals };
  }
  if (type === 'CancelOrder' || type === 'CancelAll') {
    const keep = [];
    for (const o of a.orders) {
      const m = markets.get(o.symbol);
      const hit = type === 'CancelOrder' ? o.orderId === String(msg.orderId) : Number(msg.marketId) === 0 || m.pinned.marketId === Number(msg.marketId);
      if (hit) changedOrders.push({ ...o, status: 'canceled', updatedAt: state.time });
      else keep.push(o);
    }
    if (type === 'CancelOrder' && changedOrders.length === 0) return { ok: false, error: 'ORDER_NOT_FOUND', changedOrders, fills, withdrawals };
    a.orders = keep;
    a.history = [...changedOrders, ...a.history];
    return { ok: true, error: null, changedOrders, fills, withdrawals };
  }
  if (type === 'Withdraw') {
    const amount = fromScaled(msg.amount, 6);
    const free = Number(wireAccount(a).withdrawable);
    if (amount > free) return { ok: false, error: 'INSUFFICIENT_BALANCE', changedOrders, fills, withdrawals };
    a.collateral -= amount;
    const w = withdrawal(a.address, String(80 + a.withdrawals.length), amount - WITHDRAWAL_FEE, 'requested', state.time, {});
    w.recipient = String(msg.recipient).toLowerCase();
    a.withdrawals.unshift(w);
    withdrawals.push(w);
    return { ok: true, withdrawalId: w.withdrawalId, error: null, changedOrders, fills, withdrawals };
  }
  if (type === 'ApproveAgent') {
    a.agents = [...a.agents.filter((x) => x.agent !== String(msg.agent).toLowerCase()), { agent: String(msg.agent).toLowerCase(), name: String(msg.name), approvedAt: state.time, expiresAt: Number(msg.expiresAt), height: state.height + 1 }];
    return { ok: true, error: null, changedOrders, fills, withdrawals };
  }
  if (type === 'RevokeAgent') {
    a.agents = a.agents.filter((x) => x.agent !== String(msg.agent).toLowerCase());
    return { ok: true, error: null, changedOrders, fills, withdrawals };
  }
  if (type === 'UpdateLeverage') {
    const m = marketById(msg.marketId);
    const p = a.positions.find((x) => x.symbol === m?.pinned.symbol);
    if (p) p.leverage = Number(msg.leverage);
    return { ok: true, error: null, changedOrders, fills, withdrawals };
  }
  if (type === 'UpdateMargin') {
    const m = marketById(msg.marketId);
    const p = a.positions.find((x) => x.symbol === m?.pinned.symbol && x.isolated);
    if (!p) return { ok: false, error: 'NO_ISOLATED_POSITION', changedOrders, fills, withdrawals };
    p.isolatedMargin = Math.max(0, p.isolatedMargin + (msg.add === true ? 1 : -1) * fromScaled(msg.amount, 6));
    return { ok: true, error: null, changedOrders, fills, withdrawals };
  }
  return { ok: false, error: 'UNSUPPORTED', changedOrders, fills, withdrawals };
}
function fill(a, m, buy, size, px, fills, liquidation = false) {
  const signed = buy ? size : -size;
  let p = a.positions.find((x) => x.symbol === m.pinned.symbol);
  let realized = 0;
  if (!p) {
    p = { symbol: m.pinned.symbol, size: 0, entry: px, leverage: 10, isolated: false, isolatedMargin: 0, realized: 0, funding: 0 };
    a.positions.push(p);
  }
  if (p.size === 0 || Math.sign(p.size) === Math.sign(signed)) {
    p.entry = (Math.abs(p.size) * p.entry + size * px) / (Math.abs(p.size) + size);
    p.size += signed;
  } else {
    const closing = Math.min(Math.abs(signed), Math.abs(p.size));
    realized = (p.size > 0 ? 1 : -1) * closing * (px - p.entry);
    p.size += signed;
    if (Math.abs(p.size) < m.step / 2) p.size = 0;
    else if (Math.sign(p.size) === Math.sign(signed)) p.entry = px;
  }
  p.realized += realized;
  const fee = size * px * 0.00045;
  a.collateral += realized - fee;
  a.positions = a.positions.filter((x) => x.size !== 0);
  fills.push({ fillId: String(5_600_000 + state.tradeId), tradeId: String(state.tradeId++), orderId: String(state.nextId), account: a.address, symbol: m.pinned.symbol, side: buy ? 'buy' : 'sell', price: fx(px, m.pd), size: fx(size, m.sd), fee: fx(fee, 6), liquidity: 'taker', realizedPnl: fx(realized, 6), liquidation, time: state.time, height: state.height + 1 });
  a.fills = [...fills, ...a.fills];
}

// ---------- HTTP ----------
const err = (code, message, details = {}) => ({ error: { code, message, details } });
const send = (res, code, body) => {
  res.writeHead(code, { 'content-type': 'application/json', 'access-control-allow-origin': '*', 'access-control-allow-headers': 'content-type, x-mock-country', 'cache-control': 'no-store' });
  res.end(JSON.stringify(body));
};
const SCENARIOS = {
  halt: (v) => {
    const m = markets.get(v);
    if (m) {
      m.status = 'halted';
      m.haltReason = 'Oracle unsafe: fewer than 3 independent sources agree within 2%.';
    }
  },
  resume: (v) => {
    const m = markets.get(v);
    if (m) {
      m.status = 'active';
      m.haltReason = null;
    }
  },
  closeOnly: (v) => {
    const m = markets.get(v);
    if (m) m.status = 'closeOnly';
  },
  busy: (v) => (state.busy = v === '1'),
  geo: (v) => (state.geoDefault = v === 'none' ? '' : v.toUpperCase()),
  sanction: (v) => state.sanctioned.add(v.toLowerCase()),
  vault: (v) => (state.vaultMode = v),
  sequencer: (v) => (state.sequencer = v),
  finality: (v) => (state.finalityStalled = v === 'stalled'),
  eip712: (v) => (state.eip712 = v),
  accounts: (v) => {
    state.freshAccounts = v === 'empty';
    accounts.clear();
  },
  disconnect: (v) => {
    state.refuseSockets = Number(v) || 5;
    for (const s of sockets) s.destroy();
    setTimeout(() => (state.refuseSockets = 0), state.refuseSockets * 1000);
  },
  liquidate: (v) => {
    for (const a of accounts.values()) {
      const p = a.positions.find((x) => x.symbol === v);
      if (!p) continue;
      const m = markets.get(v);
      const fills = [];
      fill(a, m, p.size < 0, Math.abs(p.size), m.mark, fills, true);
      pushAccount(a, { fills });
    }
  },
  step: (v) => {
    for (let i = 0; i < (Number(v) || 1); i++) broadcastBlock(step());
  },
};
const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  const path = url.pathname;
  if (req.method === 'OPTIONS') return send(res, 204, {});
  try {
    if (path === '/mock/scenario') {
      for (const [k, v] of url.searchParams) SCENARIOS[k]?.(v);
      broadcastStatus();
      return send(res, 200, { ok: true, state: { ...state, sanctioned: [...state.sanctioned] } });
    }
    if (req.method === 'POST' && path === '/v1/actions') {
      const chunks = [];
      for await (const c of req) chunks.push(c);
      const raw = Buffer.concat(chunks).toString('utf8');
      if (raw.length > 16_384) return send(res, 413, err('TOO_LARGE', 'Body over 16 KiB.'));
      let body;
      try {
        body = JSON.parse(raw);
      } catch {
        return send(res, 400, err('MALFORMED', 'Body is not JSON.'));
      }
      const [code, out] = await action(req, url, body);
      return send(res, code, out);
    }
    if (path === '/v1/markets') return send(res, 200, { markets: [...markets.values()].map(wireMarket) });
    if (path === '/v1/tickers') return send(res, 200, { tickers: [...markets.values()].map(wireTicker) });
    if (path === '/v1/status') return send(res, 200, status());
    if (path === '/v1/geo') return send(res, 200, geo(req, url));
    if (path === '/v1/eip712') return send(res, 200, state.eip712 === 'pinned' ? TYPES : state.eip712 === 'otherVault' ? { ...TYPES, domain: { ...TYPES.domain, verifyingContract: '0x00000000000000000000000000000000000f0f02' } } : { ...TYPES, types: { ...TYPES.types, UpdateMargin: [...TYPES.types.UpdateMargin, { name: 'memo', type: 'string' }] } });
    if (path === '/v1/committee') return send(res, 200, { threshold: 5, validators: Array.from({ length: 7 }, (_, i) => `0x${String(i + 1).repeat(40)}`), setHash: h32('committee') });
    if (path === '/v1/finality/latest') return send(res, 200, { ...blockRef(state.finalized), blockHash: h32(`b${state.finalized}`), digest: h32(`d${state.finalized}`), threshold: 5, final: true, signatures: [] });
    let m;
    if ((m = /^\/v1\/markets\/([A-Z]+-USD)(?:\/(ticker|book|trades|candles|funding))?$/.exec(path))) {
      const market = markets.get(m[1]);
      if (!market) return send(res, 404, err('NOT_FOUND', 'Unknown market.'));
      if (!m[2]) return send(res, 200, wireMarket(market));
      if (m[2] === 'ticker') return send(res, 200, wireTicker(market));
      if (m[2] === 'book') return send(res, 200, wireBook(market, Number(url.searchParams.get('depth') ?? 50)));
      if (m[2] === 'trades') return send(res, 200, { trades: market.trades.slice(0, Number(url.searchParams.get('limit') ?? 100)) });
      if (m[2] === 'candles') {
        const interval = url.searchParams.get('interval') ?? '1m';
        if (!INTERVALS[interval]) return send(res, 400, err('MALFORMED', 'Unknown interval.'));
        return send(res, 200, { candles: history(market, interval).slice(-Number(url.searchParams.get('limit') ?? 300)).map((c) => wireCandle(market, c)) });
      }
      if (m[2] === 'funding') return send(res, 200, { funding: Array.from({ length: 24 }, (_, i) => ({ symbol: m[1], time: nextFundingTime() - (i + 1) * 3_600_000, rate: fx(0.0000125 + Math.sin(i) * 0.000004, 7), premium: fx(0.000011, 7), markPrice: fx(market.mark, market.pd), indexPrice: fx(market.index, market.pd) })) });
    }
    if ((m = /^\/v1\/oracle\/([A-Z]+-USD)$/.exec(path))) return markets.has(m[1]) ? send(res, 200, oracle(markets.get(m[1]))) : send(res, 404, err('NOT_FOUND', 'Unknown market.'));
    if ((m = /^\/v1\/accounts\/(0x[0-9a-fA-F]{40})(?:\/(orders|fills|funding|deposits|withdrawals|agents|nonce))?$/.exec(path))) {
      const a = getAccount(m[1]);
      const limit = Number(url.searchParams.get('limit') ?? 100);
      switch (m[2]) {
        case undefined:
          return send(res, 200, wireAccount(a));
        case 'orders':
          return send(res, 200, { orders: url.searchParams.get('status') === 'all' ? [...a.orders, ...a.history].slice(0, limit) : a.orders.slice(0, limit) });
        case 'fills':
          return send(res, 200, { fills: a.fills.slice(0, limit) });
        case 'funding':
          return send(res, 200, { payments: a.funding.slice(0, limit) });
        case 'deposits':
          return send(res, 200, { deposits: a.deposits.map((d) => ({ ...d, account: a.address })) });
        case 'withdrawals':
          return send(res, 200, { withdrawals: a.withdrawals });
        case 'agents':
          return send(res, 200, { agents: a.agents });
        case 'nonce':
          return send(res, 200, { account: a.address, nextNonce: String(BigInt(a.nonce) + 1n), agents: [] });
      }
    }
    if ((m = /^\/v1\/actions\/(0x[0-9a-f]{64})$/.exec(path))) return acks.has(m[1]) ? send(res, 200, acks.get(m[1])) : send(res, 404, err('NOT_FOUND', 'Unknown action.'));
    return send(res, 404, err('NOT_FOUND', 'No such endpoint.'));
  } catch (e) {
    return send(res, 500, err('INTERNAL', String(e?.message ?? e).slice(0, 200)));
  }
});

// ---------- WebSocket (RFC 6455, text frames only) ----------
const sockets = new Set();
const subs = new WeakMap();
server.on('upgrade', (req, socket) => {
  if (!req.url.startsWith('/v1/ws') || state.refuseSockets) return socket.destroy();
  const key = req.headers['sec-websocket-key'];
  const accept = createHash('sha1').update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  sockets.add(socket);
  subs.set(socket, new Set());
  let buffer = Buffer.alloc(0);
  socket.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    for (;;) {
      if (buffer.length < 2) return;
      const opcode = buffer[0] & 0x0f;
      let len = buffer[1] & 0x7f;
      let at = 2;
      if (len === 126) {
        if (buffer.length < 4) return;
        len = buffer.readUInt16BE(2);
        at = 4;
      } else if (len === 127) {
        if (buffer.length < 10) return;
        len = Number(buffer.readBigUInt64BE(2));
        at = 10;
      }
      const masked = (buffer[1] & 0x80) !== 0;
      const need = at + (masked ? 4 : 0) + len;
      if (buffer.length < need) return;
      const mask = masked ? buffer.subarray(at, at + 4) : null;
      const payload = Buffer.from(buffer.subarray(at + (masked ? 4 : 0), need));
      if (mask) for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
      buffer = buffer.subarray(need);
      if (opcode === 0x8) return socket.end();
      if (opcode === 0x9) continue;
      if (opcode === 0x1) onMessage(socket, payload.toString('utf8'));
    }
  });
  const drop = () => sockets.delete(socket);
  socket.on('close', drop);
  socket.on('error', drop);
});
function frame(text) {
  const payload = Buffer.from(text, 'utf8');
  const head = payload.length < 126 ? Buffer.from([0x81, payload.length]) : payload.length < 65536 ? Buffer.from([0x81, 126, payload.length >> 8, payload.length & 255]) : Buffer.concat([Buffer.from([0x81, 127]), (() => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(payload.length)); return b; })()]);
  return Buffer.concat([head, payload]);
}
const wsSend = (socket, message) => {
  if (!socket.destroyed) socket.write(frame(JSON.stringify(message)));
};
function onMessage(socket, text) {
  let m;
  try {
    m = JSON.parse(text);
  } catch {
    return;
  }
  const set = subs.get(socket);
  if (m.op === 'ping') return wsSend(socket, { op: 'pong', time: state.time });
  const key = m.channel === 'account' ? `account:${String(m.account).toLowerCase()}` : m.channel === 'candles' ? `candles:${m.symbol}:${m.interval}` : `${m.channel}:${m.symbol ?? ''}`;
  if (m.op === 'unsubscribe') {
    set.delete(key);
    return wsSend(socket, { op: 'unsubscribed', id: m.id, channel: m.channel, symbol: m.symbol });
  }
  if (m.op !== 'subscribe') return;
  set.add(key);
  wsSend(socket, { op: 'subscribed', id: m.id, channel: m.channel, symbol: m.symbol });
  const market = markets.get(m.symbol);
  if (m.channel === 'book' && market) wsSend(socket, { channel: 'book', symbol: m.symbol, type: 'snapshot', data: wireBook(market, Number(m.depth ?? 50)) });
  if (m.channel === 'trades' && market) wsSend(socket, { channel: 'trades', symbol: m.symbol, type: 'snapshot', data: { trades: market.trades.slice(0, 50) } });
  if (m.channel === 'candles' && market && INTERVALS[m.interval]) wsSend(socket, { channel: 'candles', symbol: m.symbol, interval: m.interval, type: 'snapshot', data: { candles: history(market, m.interval).slice(-300).map((c) => wireCandle(market, c)) } });
  if (m.channel === 'ticker') for (const x of markets.values()) if (m.symbol === '*' || m.symbol === x.pinned.symbol) wsSend(socket, { channel: 'ticker', symbol: x.pinned.symbol, type: 'update', data: wireTicker(x) });
  if (m.channel === 'account') {
    const a = getAccount(String(m.account));
    wsSend(socket, { channel: 'account', account: a.address, type: 'snapshot', data: { account: wireAccount(a), orders: a.orders, agents: a.agents } });
  }
}
const subscribers = (key) => [...sockets].filter((s) => subs.get(s)?.has(key));
function pushAccount(a, parts) {
  for (const s of subscribers(`account:${a.address}`))
    wsSend(s, { channel: 'account', account: a.address, type: 'update', data: { height: state.height, time: state.time, account: wireAccount(a), ...Object.fromEntries(Object.entries(parts).filter(([, v]) => v && v.length)), agents: a.agents } });
}
function broadcastStatus() {
  for (const s of sockets) if (subs.get(s)?.has('status:')) wsSend(s, { channel: 'status', type: 'update', data: status() });
}
let tick = 0;
function broadcastBlock({ blockTrades, deltas }) {
  tick++;
  for (const [symbol, delta] of deltas) for (const s of subscribers(`book:${symbol}`)) wsSend(s, { channel: 'book', symbol, type: 'delta', data: delta });
  for (const [symbol, trades] of blockTrades) if (trades.length) for (const s of subscribers(`trades:${symbol}`)) wsSend(s, { channel: 'trades', symbol, type: 'update', data: { trades } });
  for (const m of markets.values())
    for (const interval of Object.keys(INTERVALS)) {
      const list = history(m, interval);
      for (const s of subscribers(`candles:${m.pinned.symbol}:${interval}`)) wsSend(s, { channel: 'candles', symbol: m.pinned.symbol, interval, type: 'update', data: wireCandle(m, list[list.length - 1]) });
    }
  if (tick % 5 === 0) {
    for (const m of markets.values()) for (const s of [...subscribers(`ticker:*`), ...subscribers(`ticker:${m.pinned.symbol}`)]) wsSend(s, { channel: 'ticker', symbol: m.pinned.symbol, type: 'update', data: wireTicker(m) });
    for (const a of accounts.values()) if (subscribers(`account:${a.address}`).length) pushAccount(a, {});
  }
  broadcastStatus();
}
if (!STATIC)
  setInterval(() => {
    const out = step();
    broadcastBlock(out);
    if (state.busy) for (let i = 0; i < 60; i++) broadcastBlock(step());
  }, BLOCK_MS);

server.listen(PORT, '127.0.0.1', () => console.log(`perps mock stand-in on http://127.0.0.1:${PORT} (seed ${SEED}${STATIC ? ', static' : ''})`));
