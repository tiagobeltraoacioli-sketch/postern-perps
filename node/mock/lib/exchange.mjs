// The mock exchange: a deterministic, integer-only simulation of the PosternDEX Perpetuals node behind
// docs/API.md v0.1. UNAUDITED prototype for frontend development and the shared API tests; it is NOT the engine
// (docs/SPEC.md, agent 388) and its matching, margin and liquidation rules are simplified (README "Fidelity").
//
// Determinism: the content of every block (inputs, trades, book, accounts, state root, log hash) is a function of
// (seed, start time, fixtures, the ordered list of admitted actions and scenario switches) only. Wall-clock time
// decides only WHEN a block is produced, never what it contains: block time = previous block time + blockMs.
//
// Accounting is exact: every market has priceDecimals + sizeDecimals <= 6, so price * size converts to USDC
// (6 decimals) by an exact multiplication; position cost basis is kept as an integer and split without loss, so
// the conservation invariant (exchange.conservation()) holds with equality, not within a tolerance.
import { keccak256, hex, concat } from './keccak.mjs';
import { sign, addressOf } from './secp256k1.mjs';
import { typedDataHash } from './eip712.mjs';
import { Rng } from './rng.mjs';
import { encodeInputs, ORACLE_TYPES } from './encoding.mjs';
import { parseFixed, formatFixed, pow10, divFloor, divCeil, abs, min, max, parseU64 } from './fixed.mjs';

const enc = new TextEncoder();
const USDC_DP = 6;
const RATE_DP = 10; // funding rates and premiums: fraction with 10 decimals
const FRAC_DP = 6; // margin fractions
const BPS_DP = 2; // fee bps with 2 decimals ("4.5" -> 450)
export const INTERVALS = { '1m': 60_000, '5m': 300_000, '15m': 900_000, '1h': 3_600_000, '4h': 14_400_000, '1d': 86_400_000 };
const HISTORY_CANDLES = 1500;
// System account PROTOCOL_FEE of SPEC section 8 (no key); its balance is swept to the fee recipient
// 0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544 (risk.json global.feeRecipient) by SPEC S-9.3; the mock does not sweep.
export const FEE_ACCOUNT = '0x0000000000000000000000000000000000000001';
const ZERO32 = '0x' + '00'.repeat(32);

/** A throwaway key derived from a public label: mock only, never a real key. */
export const mockKey = (label) => keccak256(enc.encode(`postern-perps-mock/${label}`));
const h32 = (s) => hex(keccak256(enc.encode(s)));
const be64 = (v) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(v));
  return b;
};
const unhex = (h) => new Uint8Array(Buffer.from(h.slice(2), 'hex'));
/** Canonical JSON: sorted keys, BigInt as decimal strings. Used for the mock's input encoding and leaves. */
export function canonical(v) {
  if (typeof v === 'bigint') return JSON.stringify(v.toString());
  if (v === null || typeof v !== 'object') return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`;
  return `{${Object.keys(v).sort().filter((k) => v[k] !== undefined).map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(',')}}`;
}

/** Commutative-pair binary Merkle tree (mock format, NOT docs/SPEC.md's): odd node carried up unchanged. */
export function merkle(leaves) {
  const layers = [leaves.map((l) => unhex(l))];
  while (layers.at(-1).length > 1) {
    const cur = layers.at(-1), next = [];
    for (let i = 0; i < cur.length; i += 2) {
      if (i + 1 === cur.length) next.push(cur[i]);
      else {
        const [a, b] = Buffer.compare(Buffer.from(cur[i]), Buffer.from(cur[i + 1])) <= 0 ? [cur[i], cur[i + 1]] : [cur[i + 1], cur[i]];
        next.push(keccak256(concat(a, b)));
      }
    }
    layers.push(next);
  }
  const root = layers.at(-1).length ? hex(layers.at(-1)[0]) : ZERO32;
  const proof = (index) => {
    const out = [];
    for (let l = 0; l < layers.length - 1; l++) {
      const sib = index ^ 1;
      if (sib < layers[l].length) out.push(hex(layers[l][sib]));
      index >>= 1;
    }
    return out;
  };
  return { root, proof };
}
export function verifyMerkle(leaf, proof, root) {
  let h = unhex(leaf);
  for (const p of proof) {
    const s = unhex(p);
    const [a, b] = Buffer.compare(Buffer.from(h), Buffer.from(s)) <= 0 ? [h, s] : [s, h];
    h = keccak256(concat(a, b));
  }
  return hex(h) === root;
}

const VAULT_DOMAIN = (vault) => ({ name: 'PosternPerpsVault', version: '1', chainId: 700770, verifyingContract: vault });
// docs/VAULT.md v0.2 section 5.2 (390, contracts 27c1969; the node 389 b170e74 signs the same 16 fields).
export const CHECKPOINT_FIELDS = [
  { name: 'height', type: 'uint64' }, { name: 'time', type: 'uint64' }, { name: 'logHash', type: 'bytes32' },
  { name: 'accountsRoot', type: 'bytes32' }, { name: 'marketsRoot', type: 'bytes32' }, { name: 'withdrawalsRoot', type: 'bytes32' },
  { name: 'nextDepositIndex', type: 'uint64' }, { name: 'nextForcedIndex', type: 'uint64' }, { name: 'nextOrderId', type: 'uint64' },
  { name: 'totalEscape', type: 'uint128' }, { name: 'dust', type: 'uint128' }, { name: 'auxHash', type: 'bytes32' },
  { name: 'depositsHead', type: 'bytes32' }, { name: 'forcedHead', type: 'bytes32' },
  { name: 'withdrawalCount', type: 'uint64' }, { name: 'withdrawalsTotal', type: 'uint64' }];
const word = (v) => {
  const b = new Uint8Array(32);
  let x = BigInt(v);
  for (let i = 31; i >= 0; i--) { b[i] = Number(x & 255n); x >>= 8n; }
  return b;
};
const be128 = (v) => word(v).slice(16);
const tag32 = (t) => { const b = new Uint8Array(32); b.set(enc.encode(t)); return b; };
/** VAULT.md 5.4 chain heads: head(0) = keccak(abi.encode(bytes32 tag, chainid, vault)). */
export const chainHead0 = (tag, vault) => hex(keccak256(concat(tag32(tag), word(700770), word(BigInt(vault)))));
/** VAULT.md 5.2 / SPEC 11: stateRoot from the first twelve checkpoint fields. */
export function stateRootOf(f) {
  const sys = keccak256(concat(enc.encode('PPv1sys'), be64(f.nextDepositIndex), be64(f.nextForcedIndex), be64(f.nextOrderId), be128(f.totalEscape), be128(f.dust), unhex(f.auxHash)));
  return hex(keccak256(concat(enc.encode('PosternPerps/state/v1'), be64(f.height), be64(f.time), unhex(f.accountsRoot), unhex(f.marketsRoot), unhex(f.withdrawalsRoot), sys)));
}
// Mock withdrawal record (VAULT.md decides the real one). PROVISIONAL.
export const WITHDRAWAL_FIELDS = [
  { name: 'withdrawalId', type: 'uint64' }, { name: 'account', type: 'address' }, { name: 'recipient', type: 'address' },
  { name: 'amount', type: 'uint64' }, { name: 'fee', type: 'uint64' }, { name: 'height', type: 'uint64' }];

const ORDER_TYPES = ['limit', 'market', 'stopMarket', 'stopLimit', 'takeProfitMarket', 'takeProfitLimit'];
const TIFS = ['GTC', 'IOC', 'POST_ONLY'];

export class EngineError extends Error {
  constructor(code, message) {
    super(message ?? code);
    this.code = code;
  }
}

export class Exchange {
  constructor({ seed = 7, startTime = 1794878400000, blockMs = 200, startHeight = 120000, marketsFx, eip712Fx, vault }) {
    this.seed = BigInt(seed);
    this.rng = new Rng(this.seed);
    this.blockMs = blockMs;
    this.eip712 = eip712Fx;
    this.vault = (vault ?? eip712Fx.domain.verifyingContract ?? addressOf(mockKey('vault'))).toLowerCase();
    this.wireScale = { price: eip712Fx.scales?.price ?? null, size: eip712Fx.scales?.size ?? null }; // SPEC 1: fixed 8
    this.collateralToken = addressOf(mockKey(`token/${seed}`));
    this.height = startHeight;
    this.time = startTime;
    this.startTime = startTime;
    this.blocks = new Map();
    this.maxBlocks = 20000;
    this.checkpoints = [];
    this.lastCheckpointTime = -Infinity;
    this.finalityStalled = false;
    this.sequencerState = 'running';
    this.vaultMode = 'normal';
    this.pending = [];
    this.acks = new Map();
    this.depositsHead = chainHead0('PosternPerps.deposit.v1', this.vault); // VAULT.md 5.4
    this.forcedHead = chainHead0('PosternPerps.forced.v1', this.vault);
    this.nonceBySigner = new Map(); // per signer: its 64 highest used nonces, ascending (SPEC 2.5)
    this.ids = { order: 9_912_000n, trade: 771_000n, fill: 5_512_000n, withdrawal: 70n, deposit: 40n, forced: 0n };
    this.depositsCredited = 0n;
    this.forcedProcessed = 0n;
    this.insurance = 0n;
    this.netDeposits = 0n;
    this.l2 = { finalizedBlock: 18200, lastDepositBlock: 18190 };
    this.oracleRound = BigInt(Math.floor(startTime / 3000));
    this.listeners = new Set();
    this.busy = false;
    this.autoDemo = true;

    // Committee: seven throwaway keys derived from the seed (API.md 10), threshold 5.
    this.committeeKeys = Array.from({ length: 7 }, (_, i) => mockKey(`committee/${seed}/${i}`));
    this.committee = this.committeeKeys.map((k, i) => ({ key: k, address: addressOf(k), i })).sort((a, b) => (a.address < b.address ? -1 : 1));
    this.threshold = 5;
    // Oracle committee (394): seven throwaway keys, quorum 5; dedicated keys, not the checkpoint keys.
    this.oracleKeys = Array.from({ length: 7 }, (_, i) => mockKey(`oracle/${seed}/${i}`));
    this.oracleSigners = this.oracleKeys.map((k) => addressOf(k));
    this.oracleQuorum = 5;
    this.sequencerKey = mockKey(`sequencer/${seed}`);
    this.sequencer = addressOf(this.sequencerKey);
    this.setHash = hex(keccak256(concat(be64(this.threshold), ...this.committee.map((c) => unhex(c.address)))));

    this.liquidity = addressOf(mockKey(`liquidity/${seed}`)); // before the markets: maker quotes belong to it
    this.markets = new Map();
    this.byId = new Map();
    const d = marketsFx.defaults;
    for (const f of marketsFx.markets) this.addMarket(f, d);

    this.accounts = new Map();
    this.demoKey = mockKey(`demo/${seed}`);
    this.demo = addressOf(this.demoKey);
    this.account(FEE_ACCOUNT);
    const liq = this.account(this.liquidity);
    this.credit(liq, parseFixed('1000000000', USDC_DP)); // the simulated market makers' and takers' collateral
    // Genesis log hash binds the parameters (API.md 3.3; SPEC decides the real encoding).
    this.genesisLogHash = h32(canonical({ mock: 'postern-perps-mock-genesis-v0', seed: this.seed, startTime, startHeight, markets: marketsFx, eip712: eip712Fx.types }));
    this.lastLogHash = this.genesisLogHash;
    this.seedDemo(this.demo);
  }

  // ---------- markets ----------
  addMarket(f, d) {
    const pd = f.priceDecimals, sd = f.sizeDecimals;
    if (pd + sd > USDC_DP) throw new Error(`${f.symbol}: priceDecimals + sizeDecimals must be <= 6 for exact USDC accounting`);
    const m = {
      cfg: { ...d, ...f, quote: d.quote ?? 'USD' },
      symbol: f.symbol, id: f.marketId, pd, sd,
      tick: parseFixed(f.tickSize, pd), step: parseFixed(f.stepSize, sd),
      minSize: parseFixed(f.minOrderSize, sd), maxSize: parseFixed(f.maxOrderSize, sd),
      maxLev: BigInt(f.maxLeverage), oiCap: parseFixed(f.openInterestCap, sd),
      tiers: f.marginTiers.map((t) => ({ floor: parseFixed(t.notionalFloor, USDC_DP), imf: parseFixed(t.initialMarginFraction, FRAC_DP), mmf: parseFixed(t.maintenanceMarginFraction, FRAC_DP), lev: t.maxLeverage, raw: t })),
      makerBps: parseFixed(f.makerFeeBps ?? d.makerFeeBps, BPS_DP), takerBps: parseFixed(f.takerFeeBps ?? d.takerFeeBps, BPS_DP), liqBps: parseFixed(f.liquidationFeeBps ?? d.liquidationFeeBps, BPS_DP),
      maxFunding: parseFixed(f.maxFundingRate ?? d.maxFundingRate, RATE_DP), interest: BigInt(f.interest ?? 12500) * 10n, // RATE_SCALE 1e9 -> mock 1e10
      slipBps: BigInt(f.marketSlippageBps ?? 500), bandBps: BigInt(f.markBandBps ?? 50),
      status: 'active', haltReason: null, oracleState: 'ok',
      rng: this.rng.fork(`market/${f.symbol}`),
      vol: BigInt(f.mockVolBps ?? 4) * 10n, // per-block uniform step bound, in millionths
      levelSize: parseFixed(f.mockLevelSize ?? '1', sd),
      bids: new Map(), asks: new Map(), seq: 88_000 + f.marketId * 1000,
      trades: [], candles: {}, funding: [], premiumSum: 0n, premiumN: 0n, fundingRate: BigInt(f.interest ?? 12500) * 10n, oi: 0n,
    };
    m.index = parseFixed(f.mockStartPrice, pd);
    m.index -= m.index % m.tick;
    m.mark = m.index;
    m.last = m.index;
    m.nextFunding = (BigInt(this.time) / 3_600_000n + 1n) * 3_600_000n;
    this.buildHistory(m);
    this.requote(m);
    this.markets.set(m.symbol, m);
    this.byId.set(m.id, m);
  }

  /** Candle history before the start time: an independent walk per interval ending at the start price. */
  buildHistory(m) {
    const r = m.rng.fork('history');
    for (const [iv, ms] of Object.entries(INTERVALS)) {
      const list = [];
      let close = m.index;
      const t0 = Math.floor(this.time / ms) * ms;
      const scale = BigInt(Math.round(Math.sqrt(ms / this.blockMs)));
      for (let i = 0; i < HISTORY_CANDLES; i++) {
        const t = t0 - (i + 1) * ms;
        const stepBound = (close * m.vol * scale) / 1_000_000n;
        const open = this.roundTick(m, close - r.range(-stepBound, stepBound));
        const hi = this.roundTick(m, max(open, close) + r.range(0n, stepBound / 2n + m.tick));
        const lo = this.roundTick(m, min(open, close) - r.range(0n, stepBound / 2n + m.tick));
        const n = Number(r.range(5n, 400n));
        const v = (m.levelSize * BigInt(n)) / 200n;
        list.push({ t, o: open, h: hi, l: max(lo, m.tick), c: close, v, q: this.usdc(m, (v * (open + close)) / 2n), n });
        close = open;
      }
      list.reverse();
      list.push({ t: t0, o: m.index, h: m.index, l: m.index, c: m.index, v: 0n, q: 0n, n: 0 });
      m.candles[iv] = list;
    }
  }
  roundTick(m, p) {
    const q = p - (((p % m.tick) + m.tick) % m.tick);
    return q < m.tick ? m.tick : q;
  }

  /** Maker ladder around the index: 30 levels per side, sizes from the market's stream. */
  requote(m) {
    const gap = max(m.tick, this.roundTick(m, (m.index * 1n) / 20000n)); // about 0.5 bps per level
    const mid = this.roundTick(m, m.index);
    const target = { bids: new Map(), asks: new Map() };
    for (let i = 0; i < 30; i++) {
      const off = gap * BigInt(i + 1);
      const sz = (side) => {
        let s = (m.levelSize * m.rng.range(20n, 180n)) / 100n;
        if (this.busy) s = (s * m.rng.range(50n, 150n)) / 100n;
        s -= s % m.step;
        return s < m.step ? m.step : s;
      };
      target.bids.set(mid - off, sz());
      target.asks.set(mid + off, sz());
    }
    for (const side of ['bids', 'asks']) {
      const book = m[side];
      for (const [px, queue] of book) {
        const mk = queue.find((o) => o.maker);
        if (mk && !target[side].has(px)) {
          queue.splice(queue.indexOf(mk), 1);
          if (!queue.length) book.delete(px);
        }
      }
      for (const [px, size] of target[side]) {
        let q = book.get(px);
        if (!q) book.set(px, (q = []));
        const mk = q.find((o) => o.maker);
        if (mk) mk.remaining = size;
        else q.unshift({ maker: true, account: this.liquidity, remaining: size, isBuy: side === 'bids' });
      }
    }
    // Makers never cross the book: drop maker levels at or through the opposite best user level.
    this.uncross(m);
  }
  uncross(m) {
    for (;;) {
      const bb = this.best(m, 'bids'), ba = this.best(m, 'asks');
      if (bb === null || ba === null || bb < ba) return;
      const bq = m.bids.get(bb), aq = m.asks.get(ba);
      const bm = bq.find((o) => o.maker), am = aq.find((o) => o.maker);
      if (am) {
        aq.splice(aq.indexOf(am), 1);
        if (!aq.length) m.asks.delete(ba);
      } else if (bm) {
        bq.splice(bq.indexOf(bm), 1);
        if (!bq.length) m.bids.delete(bb);
      } else return; // two user orders cannot rest crossed (matching prevents it)
    }
  }
  best(m, side) {
    let b = null;
    for (const px of m[side].keys()) if (b === null || (side === 'bids' ? px > b : px < b)) b = px;
    return b;
  }
  levels(m, side, depth = Infinity) {
    const px = [...m[side].keys()].sort((a, b) => (side === 'bids' ? (a > b ? -1 : 1) : a < b ? -1 : 1));
    const out = [];
    for (const p of px) {
      const total = m[side].get(p).reduce((s, o) => s + o.remaining, 0n);
      if (total > 0n) out.push([p, total]);
      if (out.length >= depth) break;
    }
    return out;
  }

  // ---------- accounts ----------
  account(address) {
    const a = address.toLowerCase();
    let acc = this.accounts.get(a);
    if (!acc) {
      acc = { address: a, collateral: 0n, positions: new Map(), orders: new Map(), history: [], fills: [], funding: [], deposits: [], withdrawals: [], agents: new Map(), volume30d: 0n, leverage: new Map(), seeded: false };
      this.accounts.set(a, acc);
    }
    return acc;
  }
  /** Read access used by the API: with autoDemo on, an unknown address gets the demo data (mock convenience). */
  accountForRead(address) {
    const a = address.toLowerCase();
    if (!this.accounts.has(a) && this.autoDemo && a !== FEE_ACCOUNT) this.seedDemo(a);
    return this.account(a);
  }
  credit(acc, amount) {
    acc.collateral += amount;
    this.netDeposits += amount;
  }
  position(acc, m) {
    let p = acc.positions.get(m.id);
    if (!p) acc.positions.set(m.id, (p = { size: 0n, cost: 0n, realized: 0n, funding: 0n, mode: 'cross', isolatedMargin: 0n, openedAt: this.time }));
    return p;
  }
  leverageOf(acc, m) {
    return acc.leverage.get(m.id) ?? min(10n, m.maxLev);
  }
  /** price * size (market scales) to USDC with 6 decimals: exact, since pd + sd <= 6. */
  usdc(m, priceTimesSize) {
    return priceTimesSize * pow10(USDC_DP - m.pd - m.sd);
  }
  notional(m, size, price) {
    return this.usdc(m, abs(size) * price);
  }
  tier(m, notional) {
    let t = m.tiers[0];
    for (const x of m.tiers) if (notional >= x.floor) t = x;
    return t;
  }
  /** Signed cost basis: +cost for a long, -cost for a short (so uPnL = size * price - signedCost, exactly). */
  signedCost(p) {
    return p.size >= 0n ? p.cost : -p.cost;
  }
  upnl(m, p, price = m.mark) {
    return this.usdc(m, p.size * price - this.signedCost(p));
  }
  margins(acc) {
    let upnl = 0n, im = 0n, mm = 0n;
    for (const [id, p] of acc.positions) {
      if (p.size === 0n) continue;
      const m = this.byId.get(id);
      const n = this.notional(m, p.size, m.mark);
      const t = this.tier(m, n);
      const imf = max(t.imf, divCeil(pow10(FRAC_DP), this.leverageOf(acc, m)));
      upnl += this.upnl(m, p);
      im += divCeil(n * imf, pow10(FRAC_DP)); // margin rounded up: against the account
      mm += divCeil(n * t.mmf, pow10(FRAC_DP));
    }
    const isolated = [...acc.positions.values()].reduce((s, p) => s + p.isolatedMargin, 0n);
    const equity = acc.collateral + isolated + upnl;
    return { upnl, im, mm, equity, isolated };
  }
  /** Liquidation price of one position holding every other one still (an estimate, API.md 4). */
  liquidationPrice(acc, m, p) {
    if (p.size === 0n) return null;
    const g = this.margins(acc);
    const n = this.notional(m, p.size, m.mark);
    const mmf = this.tier(m, n).mmf;
    const own = this.upnl(m, p);
    const ownMm = divCeil(n * mmf, pow10(FRAC_DP));
    const e0 = g.equity - own - (g.mm - ownMm); // equity without this position minus the others' maintenance
    // e0 + usdc(s*P - sc) = mmf * usdc(|s| * P)  =>  P * k * (s - mmf*|s|) = k * sc - e0, k = 10^(6-pd-sd)
    const k = pow10(USDC_DP - m.pd - m.sd), F = pow10(FRAC_DP);
    let num = (k * this.signedCost(p) - e0) * F;
    let den = k * (p.size * F - mmf * abs(p.size));
    if (den === 0n) return null;
    if (den < 0n) {
      num = -num;
      den = -den;
    }
    const px = p.size > 0n ? divCeil(num, den) : divFloor(num, den); // rounded toward the danger side
    return px > 0n ? px : null;
  }

  // ---------- demo data ----------
  seedDemo(address) {
    const acc = this.account(address);
    if (acc.seeded) return acc;
    acc.seeded = true;
    const r = this.rng.fork(`demo/${address}`);
    this.credit(acc, parseFixed('25000', USDC_DP));
    const btc = this.byId.get(1), eth = this.byId.get(2), sol = this.byId.get(3);
    const t = this.time;
    // Positions are opened against the liquidity account so the book of positions sums to zero.
    const open = (m, size, price) => this.settleFill(m, acc, this.account(this.liquidity), size, price, { taker: true, quiet: false, time: t - 3_600_000 });
    if (btc) open(btc, parseFixed('0.5', btc.sd), this.roundTick(btc, (btc.index * 993n) / 1000n));
    if (eth) {
      open(eth, -parseFixed('4.2', eth.sd), this.roundTick(eth, (eth.index * 1011n) / 1000n));
      const p = this.position(acc, eth);
      p.mode = 'isolated';
      const move = parseFixed('1500', USDC_DP);
      acc.collateral -= move;
      p.isolatedMargin += move;
    }
    if (sol) acc.leverage.set(sol.id, 5n);
    // Orders: a resting limit (partly filled), a post-only ask, an untriggered stop, and closed history.
    const place = (m, isBuy, type, tif, price, size, extra = {}) => {
      const o = this.newOrder(acc, m, { isBuy, type, tif, price, size, reduceOnly: false, trigger: null, clientOrderId: null, ...extra });
      o.createdAt = t - Number(r.range(60_000n, 3_000_000n));
      o.updatedAt = o.createdAt;
      return o;
    };
    if (btc) {
      const o = place(btc, true, 'limit', 'GTC', this.roundTick(btc, (btc.index * 990n) / 1000n), parseFixed('0.25', btc.sd));
      o.filled = parseFixed('0.1', btc.sd);
      o.filledCost = o.filled * o.price;
      this.rest(btc, o);
      const stop = place(btc, false, 'stopMarket', 'GTC', this.roundTick(btc, (btc.index * 940n) / 1000n), parseFixed('0.5', btc.sd), { reduceOnly: true, trigger: this.roundTick(btc, (btc.index * 950n) / 1000n) });
      stop.status = 'untriggered';
      stop.triggerCondition = 'markBelow';
      acc.orders.set(stop.orderId, stop);
    }
    if (eth) this.rest(eth, place(eth, false, 'limit', 'POST_ONLY', this.roundTick(eth, (eth.index * 1020n) / 1000n), parseFixed('1.5', eth.sd)));
    for (const [st, reason] of [['filled', null], ['canceled', null], ['rejected', 'INSUFFICIENT_MARGIN'], ['expired', null]]) {
      const m = sol ?? btc;
      const o = place(m, st !== 'canceled', 'limit', st === 'expired' ? 'IOC' : 'GTC', m.index, m.minSize * 10n);
      o.status = st;
      o.rejectReason = reason;
      if (st === 'filled') {
        o.filled = o.size;
        o.filledCost = o.size * o.price;
      }
      acc.history.unshift(o);
    }
    // Funding payments of the last six hours (synthetic history).
    if (btc) {
      for (let i = 6; i >= 1; i--) {
        const time = Number(btc.nextFunding) - (i + 1) * 3_600_000;
        acc.funding.unshift({ symbol: btc.symbol, time, rate: 125_000n, positionSize: parseFixed('0.5', btc.sd), markPrice: btc.index, payment: -divCeil(this.notional(btc, parseFixed('0.5', btc.sd), btc.index) * 125_000n, pow10(RATE_DP)), m: btc });
      }
    }
    // Deposits in every status, and withdrawals in every status (certificates come with the next checkpoint).
    const dep = (amount, status, ago) => {
      const id = ++this.ids.deposit;
      const d = { depositId: id, account: acc.address, amount: parseFixed(amount, USDC_DP), l2TxHash: h32(`l2tx/dep/${address}/${id}`), logIndex: 3, l2Block: this.l2.lastDepositBlock - ago, status, creditedHeight: status === 'credited' ? this.height - 200 : null, time: t - ago * 60_000, demo: true };
      acc.deposits.unshift(d);
      return d;
    };
    dep('25000', 'credited', 40);
    dep('500', 'l2Finalized', 2);
    dep('250', 'seen', 0);
    for (const [i, st] of ['paid', 'delayed', 'submitted', 'certified', 'finalized', 'included', 'requested', 'rejected'].entries()) {
      const id = ++this.ids.withdrawal;
      const amount = st === 'delayed' ? parseFixed('150000', USDC_DP) : parseFixed(String(100 * (i + 1)), USDC_DP);
      acc.withdrawals.unshift({ withdrawalId: id, account: acc.address, recipient: acc.address, amount, fee: parseFixed('1', USDC_DP), status: st === 'requested' || st === 'rejected' ? st : 'included', target: st, demo: true, requestedHeight: this.height - 100 + i, checkpointHeight: null, certificate: null, l2TxHash: null, availableAt: null, time: t - (8 - i) * 600_000 });
    }
    if (r.chance(1, 1)) {
      const agent = addressOf(mockKey(`demo-agent/${address}`));
      acc.agents.set(agent, { agent, name: 'trading-bot', approvedAt: t - 86_400_000, expiresAt: t + 30 * 86_400_000, height: this.height - 4000 });
    }
    return acc;
  }

  // ---------- orders and matching ----------
  newOrder(acc, m, { isBuy, type, tif, price, size, reduceOnly, trigger, clientOrderId }) {
    const o = { orderId: ++this.ids.order, clientOrderId, account: acc.address, m, isBuy, type, tif, price, size, filled: 0n, filledCost: 0n, reduceOnly, triggerPrice: trigger, triggerCondition: null, status: 'open', createdAt: this.time, updatedAt: this.time, height: this.height, rejectReason: null, remaining: size };
    return o;
  }
  rest(m, o) {
    o.remaining = o.size - o.filled;
    const side = o.isBuy ? 'bids' : 'asks';
    let q = m[side].get(o.price);
    if (!q) m[side].set(o.price, (q = []));
    q.push(o);
    this.account(o.account).orders.set(o.orderId, o);
  }
  unrest(o) {
    const m = o.m, side = o.isBuy ? 'bids' : 'asks';
    const q = m[side].get(o.price);
    if (!q) return;
    const i = q.indexOf(o);
    if (i >= 0) q.splice(i, 1);
    if (!q.length) m[side].delete(o.price);
  }
  close(acc, o, status, ev) {
    if (o.status === 'open') this.unrest(o);
    acc.orders.delete(o.orderId);
    o.status = status;
    o.updatedAt = this.time;
    acc.history.unshift(o);
    ev.orders(acc, o);
  }

  /**
   * One fill between a taker and a maker account: positions, exact cost basis, realized PnL, fees, trade record.
   * `size` is signed from the taker's side (+ = taker buys).
   */
  settleFill(m, taker, maker, size, price, { taker: _t = true, liquidation = false, ev = null, takerOrder = null, makerOrder = null, time = this.time } = {}) {
    const n = this.notional(m, size, price);
    const takerFee = divCeil(n * (liquidation ? 0n : m.takerBps), 10_000n * pow10(BPS_DP));
    const makerFee = m.makerBps >= 0n ? divCeil(n * m.makerBps, 10_000n * pow10(BPS_DP)) : -divFloor(n * -m.makerBps, 10_000n * pow10(BPS_DP));
    const liqFee = liquidation ? divCeil(n * m.liqBps, 10_000n * pow10(BPS_DP)) : 0n;
    const tradeId = ++this.ids.trade;
    const out = [];
    for (const [acc, delta, fee, liq, order] of [[taker, size, takerFee + liqFee, 'taker', takerOrder], [maker, -size, makerFee, 'maker', makerOrder]]) {
      const p = this.position(acc, m);
      let realized = 0n;
      if (p.size !== 0n && (p.size > 0n) !== (delta > 0n)) {
        const closing = min(abs(delta), abs(p.size));
        const costPortion = (p.cost * closing) / abs(p.size); // floor; the remainder stays in the basis (no loss)
        const proceeds = closing * price;
        realized = p.size > 0n ? proceeds - costPortion : costPortion - proceeds;
        p.cost -= costPortion;
        p.size += p.size > 0n ? -closing : closing;
        const rest = abs(delta) - closing;
        if (rest > 0n) {
          p.size = delta > 0n ? rest : -rest;
          p.cost = rest * price;
          p.openedAt = time;
          p.funding = 0n;
        }
      } else {
        if (p.size === 0n) {
          p.openedAt = time;
          p.funding = 0n;
        }
        p.size += delta;
        p.cost += abs(delta) * price;
      }
      if (p.size === 0n) {
        p.cost = 0n;
        if (p.isolatedMargin) {
          acc.collateral += p.isolatedMargin;
          p.isolatedMargin = 0n;
        }
      }
      const r6 = this.usdc(m, realized);
      p.realized += r6;
      acc.collateral += r6 - fee;
      acc.volume30d += n;
      if (fee > 0n || fee < 0n) {
        if (liq === 'taker' && liqFee) {
          this.insurance += liqFee;
          this.account(FEE_ACCOUNT).collateral += fee - liqFee;
        } else this.account(FEE_ACCOUNT).collateral += fee;
      }
      if (acc.address !== this.liquidity && acc.address !== FEE_ACCOUNT) {
        const f = { fillId: ++this.ids.fill, tradeId, orderId: order?.orderId ?? ++this.ids.order, account: acc.address, m, isBuy: delta > 0n, price, size: abs(delta), fee, liquidity: liq, realizedPnl: r6, liquidation, time, height: this.height };
        acc.fills.unshift(f);
        if (acc.fills.length > 2000) acc.fills.length = 2000;
        out.push(f);
        ev?.fills(acc, f);
      }
    }
    const trade = { tradeId, m, price, size: abs(size), isBuy: size > 0n, time, height: this.height, liquidation };
    return { trade, fills: out };
  }

  /** Match an incoming taker order against the book (price-time priority). Returns the filled size. */
  match(m, acc, o, limitPrice, ev, { liquidation = false } = {}) {
    const side = o.isBuy ? 'asks' : 'bids';
    let filled = 0n;
    const want = o.size - o.filled;
    while (filled < want) {
      const px = this.best(m, side);
      if (px === null) break;
      if (limitPrice !== null && (o.isBuy ? px > limitPrice : px < limitPrice)) break;
      const q = m[side].get(px);
      const mk = q[0];
      if (mk.account === acc.address && !mk.maker) {
        // Self-trade prevention: cancel the resting order of the same account (expire maker).
        this.close(acc, mk, 'canceled', ev);
        continue;
      }
      const take = min(want - filled, mk.remaining);
      const makerAcc = this.account(mk.account);
      if (mk.maker && acc.address === this.liquidity) {
        // Simulated flow against a simulated quote: both sides are the liquidity account, so the trade is
        // printed for the tape and candles but settles nothing (it would be a self-trade with zero net effect).
        ev.trade(m, { tradeId: ++this.ids.trade, m, price: px, size: take, isBuy: o.isBuy, time: this.time, height: this.height, liquidation: false });
      } else {
        const { trade } = this.settleFill(m, acc, makerAcc, o.isBuy ? take : -take, px, { ev, takerOrder: o.sim ? null : o, makerOrder: mk.maker ? null : mk, liquidation });
        ev.trade(m, trade);
      }
      mk.remaining -= take;
      if (!mk.maker) {
        mk.filled += take;
        mk.filledCost += take * px;
        mk.updatedAt = this.time;
        if (mk.remaining === 0n) this.close(makerAcc, mk, 'filled', ev);
        else ev.orders(makerAcc, mk);
      }
      if (mk.remaining === 0n && mk.maker) q.shift();
      if (!q.length) m[side].delete(px);
      filled += take;
      o.filled += take;
      o.filledCost += take * px;
      m.last = px;
    }
    return filled;
  }

  // ---------- actions ----------
  /** Engine-side checks and effects of an admitted action. Throws EngineError for a business rejection. */
  apply(action, ev) {
    const { type, msg, account } = action;
    const acc = this.account(account);
    if (!acc.seeded && this.autoDemo && account !== FEE_ACCOUNT && account !== this.liquidity) this.seedDemo(account);
    switch (type) {
      case 'PlaceOrder': return this.placeOrder(acc, msg, ev);
      case 'CancelOrder': {
        const o = acc.orders.get(BigInt(msg.orderId));
        if (!o || o.m.id !== Number(msg.marketId)) throw new EngineError('ORDER_NOT_FOUND');
        this.close(acc, o, 'canceled', ev);
        return { ok: true, orderId: String(o.orderId), orderStatus: 'canceled' };
      }
      case 'CancelAll': {
        let n = 0;
        for (const o of [...acc.orders.values()]) if (Number(msg.marketId) === 0 || o.m.id === Number(msg.marketId)) { this.close(acc, o, 'canceled', ev); n++; }
        return { ok: true };
      }
      case 'UpdateLeverage': {
        const m = this.marketById(msg.marketId);
        if (m.status === 'halted') throw new EngineError('MARKET_HALTED');
        const lev = BigInt(msg.leverage);
        if (lev < 1n || lev > m.maxLev) throw new EngineError('LEVERAGE');
        const p = this.position(acc, m);
        const mode = msg.isolated ? 'isolated' : 'cross';
        if (p.size !== 0n && p.mode !== mode) throw new EngineError('LEVERAGE', 'margin mode changes only without a position');
        const prev = acc.leverage.get(m.id);
        acc.leverage.set(m.id, lev);
        p.mode = mode;
        const g = this.margins(acc);
        if (g.im > g.equity && p.size !== 0n) {
          if (prev === undefined) acc.leverage.delete(m.id);
          else acc.leverage.set(m.id, prev);
          throw new EngineError('INSUFFICIENT_MARGIN');
        }
        ev.account(acc);
        return { ok: true };
      }
      case 'UpdateMargin': {
        const m = this.marketById(msg.marketId);
        if (m.status === 'halted') throw new EngineError('MARKET_HALTED');
        const p = this.position(acc, m);
        if (p.mode !== 'isolated' || p.size === 0n) throw new EngineError('NOT_ISOLATED');
        const amount = BigInt(msg.amount);
        if ('add' in msg ? msg.add : amount > 0n) {
          const a = abs(amount);
          if (a > this.withdrawable(acc)) throw new EngineError('INSUFFICIENT_MARGIN');
          acc.collateral -= a;
          p.isolatedMargin += a;
        } else {
          const a = abs(amount);
          if (a > p.isolatedMargin) throw new EngineError('INSUFFICIENT_MARGIN');
          p.isolatedMargin -= a;
          acc.collateral += a;
          const g = this.margins(acc);
          if (g.im > g.equity) {
            p.isolatedMargin += a;
            acc.collateral -= a;
            throw new EngineError('INSUFFICIENT_MARGIN');
          }
        }
        ev.account(acc);
        return { ok: true };
      }
      case 'Withdraw': {
        const amount = BigInt(msg.amount);
        const fee = parseFixed(this.markets.values().next().value.cfg.withdrawalFee ?? '1', USDC_DP);
        if (amount <= fee) throw new EngineError('WITHDRAW_TOO_SMALL');
        if (/^0x0{40}$/.test(msg.recipient)) throw new EngineError('MALFORMED', 'recipient is the zero address');
        for (const [id, p] of acc.positions) if (p.size !== 0n && this.byId.get(id).status === 'halted') throw new EngineError('POSITION_IN_HALTED_MARKET');
        if (amount > this.withdrawable(acc)) throw new EngineError('INSUFFICIENT_MARGIN');
        acc.collateral -= amount;
        this.account(FEE_ACCOUNT).collateral += fee;
        this.netDeposits -= amount - fee;
        const w = { withdrawalId: ++this.ids.withdrawal, account: acc.address, recipient: msg.recipient.toLowerCase(), amount: amount - fee, fee, status: 'included', requestedHeight: this.height, checkpointHeight: null, certificate: null, l2TxHash: null, availableAt: null, time: this.time };
        acc.withdrawals.unshift(w);
        ev.withdrawals(acc, w);
        ev.account(acc);
        return { ok: true, withdrawalId: String(w.withdrawalId) };
      }
      case 'ApproveAgent': {
        const agent = msg.agent.toLowerCase();
        if (agent === acc.address) throw new EngineError('AGENT_INVALID');
        const exp = BigInt(msg.expiresAt);
        if (exp <= BigInt(this.time) || exp > BigInt(this.time) + 180n * 86_400_000n) throw new EngineError('MALFORMED', 'expiresAt must be in (T, T + 180 days]');
        if (this.accounts.has(agent) && agent !== acc.address) throw new EngineError('AGENT_INVALID', 'agent address is an existing account');
        for (const other of this.accounts.values()) if (other !== acc && other.agents.has(agent)) throw new EngineError('AGENT_INVALID', 'agent of another account');
        for (const [k, g] of acc.agents) if (g.expiresAt <= this.time) acc.agents.delete(k);
        if (!acc.agents.has(agent) && acc.agents.size >= 4) throw new EngineError('AGENT_INVALID', 'at most 4 agents');
        if (Buffer.byteLength(msg.name, 'utf8') > 32) throw new EngineError('MALFORMED', 'agent name over 32 bytes');
        acc.agents.set(agent, { agent, name: msg.name, approvedAt: this.time, expiresAt: Number(msg.expiresAt), height: this.height });
        ev.agents(acc);
        return { ok: true };
      }
      case 'RevokeAgent': {
        const agent = msg.agent.toLowerCase();
        if (!acc.agents.delete(agent)) throw new EngineError('AGENT_INVALID', 'not an agent of this account');
        ev.agents(acc);
        return { ok: true };
      }
      default:
        throw new EngineError('MALFORMED');
    }
  }
  marketById(id) {
    const m = this.byId.get(Number(id));
    if (!m) throw new EngineError('UNKNOWN_MARKET');
    return m;
  }
  /**
   * A signed integer in SPEC's fixed scale (1e-8) to the market's internal scale (10^-priceDecimals or
   * 10^-sizeDecimals). Exact or refused: a value finer than the tick / step is TICK / STEP, never rounded.
   */
  fromWire(m, kind, v) {
    const scale = this.wireScale[kind];
    const internal = kind === 'price' ? m.pd : m.sd;
    const raw = BigInt(v);
    if (scale === null) return raw;
    const k = pow10(scale - internal);
    if (raw % k !== 0n) throw new EngineError(kind === 'price' ? 'TICK' : 'STEP');
    return raw / k;
  }
  toWire(m, kind, v) {
    const scale = this.wireScale[kind];
    return scale === null ? v : v * pow10(scale - (kind === 'price' ? m.pd : m.sd));
  }
  withdrawable(acc) {
    const g = this.margins(acc);
    const free = acc.collateral + g.upnl - g.im;
    return max(0n, min(free, acc.collateral));
  }
  placeOrder(acc, msg, ev) {
    const m = this.marketById(msg.marketId);
    const type = ORDER_TYPES[Number(msg.orderType)];
    const tif = TIFS[Number(msg.timeInForce)];
    if (!type || !tif) throw new EngineError('MALFORMED', 'unknown orderType or timeInForce');
    const [price, size, trigger] = [this.fromWire(m, 'price', msg.price), this.fromWire(m, 'size', msg.size), this.fromWire(m, 'price', msg.triggerPrice)];
    const isBuy = msg.isBuy;
    if (m.status === 'halted' || m.status === 'settled') throw new EngineError('MARKET_HALTED');
    if (m.status === 'closeOnly' && !msg.reduceOnly) throw new EngineError('MARKET_HALTED', 'close-only: reduce-only orders only');
    if (size <= 0n || size % m.step !== 0n) throw new EngineError('STEP');
    if (size < m.minSize || size > m.maxSize) throw new EngineError('SIZE_BOUNDS');
    // Market orders carry a worst price (agreed 391/389 in the cross-area log; price 0 = "any price" refused).
    if (price <= 0n) throw new EngineError('MALFORMED', 'price is mandatory: a limit or worst price (SPEC 2.3)');
    if (price % m.tick !== 0n) throw new EngineError('TICK');
    const isTrigger = type.startsWith('stop') || type.startsWith('takeProfit');
    if (isTrigger && trigger <= 0n) throw new EngineError('MALFORMED', 'triggerPrice >= 1 for trigger types');
    if (isTrigger && trigger % m.tick !== 0n) throw new EngineError('TICK');
    if (!isTrigger && trigger !== 0n) throw new EngineError('MALFORMED', 'triggerPrice is 0 for limit and market');
    const marketType = type === 'market' || type === 'stopMarket' || type === 'takeProfitMarket';
    if (marketType && tif !== 'IOC') throw new EngineError('MALFORMED', 'market types must be IOC (SPEC 2.3)');
    const p = this.position(acc, m);
    let sz = size;
    if (msg.reduceOnly) {
      if (p.size === 0n || (p.size > 0n) === isBuy) throw new EngineError('REDUCE_ONLY');
      sz = min(size, abs(p.size));
    }
    const o = this.newOrder(acc, m, { isBuy, type, tif, price, size: sz, reduceOnly: msg.reduceOnly, trigger: isTrigger ? trigger : null, clientOrderId: msg.clientOrderId === ZERO32 ? null : msg.clientOrderId });
    if (!msg.reduceOnly) this.checkMargin(acc, m, isBuy ? sz : -sz, price);
    if (isTrigger) {
      const above = (type.startsWith('stop') && isBuy) || (type.startsWith('takeProfit') && !isBuy);
      o.triggerCondition = above ? 'markAbove' : 'markBelow';
      o.status = 'untriggered';
      acc.orders.set(o.orderId, o);
      ev.orders(acc, o);
      return { ok: true, orderId: String(o.orderId), orderStatus: 'untriggered', filledSize: '0' };
    }
    return this.execute(acc, m, o, ev);
  }
  checkMargin(acc, m, delta, price) {
    const p = this.position(acc, m);
    const g = this.margins(acc);
    const newSize = p.size + delta;
    if (abs(newSize) <= abs(p.size) && (newSize === 0n || (newSize > 0n) === (p.size > 0n))) return; // reduces
    const n = this.notional(m, newSize, max(price, m.mark));
    const t = this.tier(m, n);
    const imf = max(t.imf, divCeil(pow10(FRAC_DP), this.leverageOf(acc, m)));
    const oldN = this.notional(m, p.size, m.mark);
    const oldT = this.tier(m, oldN);
    const oldImf = max(oldT.imf, divCeil(pow10(FRAC_DP), this.leverageOf(acc, m)));
    const im = g.im - divCeil(oldN * oldImf, pow10(FRAC_DP)) + divCeil(n * imf, pow10(FRAC_DP));
    const fee = divCeil(this.notional(m, delta, price) * m.takerBps, 10_000n * pow10(BPS_DP));
    if (im + fee > g.equity) throw new EngineError('INSUFFICIENT_MARGIN');
    let oi = 0n;
    for (const a of this.accounts.values()) {
      const q = a.positions.get(m.id);
      if (q && q.size > 0n) oi += q.size;
    }
    if (newSize > 0n && oi + delta > m.oiCap && delta > 0n && acc.address !== this.liquidity) throw new EngineError('OI_CAP');
  }
  execute(acc, m, o, ev) {
    let limit = o.price;
    const isMkt = o.type === 'market' || o.type === 'stopMarket' || o.type === 'takeProfitMarket';
    if (isMkt) {
      // S-4.3: a market order never fills beyond marketSlippageBps from the mark; the tighter bound wins.
      const band = (m.mark * m.slipBps) / 10_000n;
      limit = o.isBuy ? min(limit, m.mark + band) : max(limit, m.mark - band);
    }
    if (o.tif === 'POST_ONLY') {
      const opp = this.best(m, o.isBuy ? 'asks' : 'bids');
      if (opp !== null && (o.isBuy ? opp <= limit : opp >= limit)) {
        o.status = 'rejected';
        o.rejectReason = 'WOULD_CROSS';
        acc.history.unshift(o);
        ev.orders(acc, o);
        throw new EngineError('WOULD_CROSS');
      }
    }
    this.match(m, acc, o, limit, ev);
    const isMarket = o.type === 'market' || o.type === 'stopMarket' || o.type === 'takeProfitMarket';
    if (o.filled === o.size) {
      o.status = 'filled';
      acc.history.unshift(o);
    } else if (isMarket || o.tif === 'IOC') {
      o.status = o.filled > 0n ? 'filled' : 'canceled';
      if (o.filled > 0n) o.size = o.filled; // the unfilled rest of an IOC or market order is canceled
      acc.history.unshift(o);
    } else {
      o.status = 'open';
      this.rest(m, o);
    }
    o.updatedAt = this.time;
    ev.orders(acc, o);
    ev.account(acc);
    ev.book(m);
    return { ok: true, orderId: String(o.orderId), orderStatus: o.status, filledSize: formatFixed(o.filled, m.sd) };
  }

  // ---------- blocks ----------
  /** Produce one block. Returns the event bundle for the WebSocket layer, or null when the sequencer is stopped. */
  produceBlock() {
    if (this.sequencerState !== 'running') return null;
    const ev = this.newEvents();
    const time = this.time + this.blockMs;
    const height = this.height + 1;
    this.height = height;
    const inputs = [];
    const before = new Map([...this.markets.values()].map((m) => [m, this.snapshotLevels(m)]));
    // Oracle: a new round every 3 s of engine time (394's round rule: timeMs / 3000).
    const round = BigInt(Math.floor(time / 3000));
    this.time = time;
    for (const m of this.markets.values()) {
      if (m.oracleState === 'ok' || m.oracleState === undefined) {
        const step = m.rng.range(-m.vol, m.vol);
        m.index = this.roundTick(m, m.index + (m.index * step) / 1_000_000n);
      }
    }
    if (round > this.oracleRound) {
      this.oracleRound = round;
      const live = [...this.markets.values()].filter((m) => m.oracleState === 'ok').sort((a, b) => a.id - b.id);
      if (live.length) inputs.push({ kind: 'oracle', payload: this.oracleUpdate(round, time, live) });
    }
    // Deposits: l2Finalized become credited (deposit inputs, vault index order).
    for (const acc of this.accounts.values()) {
      for (const d of acc.deposits) {
        if (d.status === 'l2Finalized' && !d.demo && d.readyAt <= time) {
          d.status = 'credited';
          d.creditedHeight = height;
          this.credit(acc, d.amount);
          this.depositsHead = hex(keccak256(concat(unhex(this.depositsHead), word(this.depositsCredited), word(BigInt(acc.address)), word(d.amount))));
          this.depositsCredited += 1n;
          inputs.push({ kind: 'deposit', payload: { depositIndex: String(this.depositsCredited - 1n), account: acc.address, amount: String(d.amount) } });
          ev.deposits(acc, d);
          ev.account(acc);
        } else if (d.status === 'seen' && !d.demo && d.finalAt <= time) {
          d.status = 'l2Finalized';
          d.readyAt = time + 2 * this.blockMs;
          ev.deposits(acc, d);
        }
      }
    }
    // Actions, in arrival order.
    const batch = this.pending.splice(0, this.pending.length);
    for (const [index, a] of batch.entries()) {
      inputs.push({ kind: 'action', account: a.account, signer: a.signer, type: a.type, message: a.wireMessage, signature: a.signature });
      let result;
      try {
        result = { error: null, ...this.apply(a, ev) };
      } catch (e) {
        if (!(e instanceof EngineError)) throw e;
        result = { ok: false, error: e.code };
      }
      const ack = { actionHash: a.actionHash, status: 'included', receivedAt: a.receivedAt, height, index: inputs.length - 1, result };
      this.acks.set(a.actionHash, ack);
      ev.ack(this.account(a.account), ack);
      a.resolve?.(ack);
    }
    // Market activity: makers requote, simulated takers trade, triggers, liquidations, funding.
    for (const m of this.markets.values()) {
      if (m.status === 'active' || m.status === 'closeOnly') {
        this.requote(m);
        const takers = this.busy ? 3 : m.rng.chance(35, 100) ? 1 : 0;
        for (let i = 0; i < takers && m.status === 'active'; i++) this.simTaker(m, ev);
      }
      this.updateMark(m);
      this.triggers(m, ev);
    }
    this.liquidations(ev);
    for (const m of this.markets.values()) {
      ev.bookDelta(m, before.get(m), this.snapshotLevels(m));
      this.candleTick(m, ev);
    }
    this.fundingTick(ev);
    // SPEC 3.2 canonical bytes. A mock block may carry no input (the simulated market moves without inputs:
    // the mock's blocks are not replayable from the log; README "Fidelity").
    const inputsEncoded = hex(encodeInputs(inputs, this.eip712));
    const prevLogHash = this.lastLogHash;
    const logHash = hex(keccak256(concat(unhex(prevLogHash), be64(height), be64(time), keccak256(unhex(inputsEncoded)))));
    const leaves = this.accountLeaves();
    const st = this.stateFields(height, time, logHash, leaves);
    const stateRoot = st.stateRoot;
    const hash = hex(keccak256(concat(unhex(logHash), unhex(stateRoot))));
    const block = { height, time, prevLogHash, logHash, stateRoot, hash, inputs, inputsEncoded, leaves, st, _sig: null };
    this.lastLogHash = logHash;
    this.blocks.set(height, block);
    if (this.blocks.size > this.maxBlocks) this.blocks.delete(height - this.maxBlocks);
    // Checkpoint every second of engine time, for the block two heights back (signature collection lag).
    if (!this.finalityStalled && time - this.lastCheckpointTime >= 1000 && this.blocks.has(height - 2)) {
      this.lastCheckpointTime = time;
      this.checkpoint(this.blocks.get(height - 2), ev);
    }
    this.progressWithdrawals(ev);
    ev.block = { height, time };
    return ev;
  }
  /** 394's OracleUpdate: `quorum` observations (signer indices ascending, rotating), each signed in the engine domain. */
  oracleUpdate(round, time, live) {
    const start = Number(round % BigInt(this.oracleKeys.length));
    const idx = Array.from({ length: this.oracleQuorum }, (_, k) => (start + k) % this.oracleKeys.length).sort((a, b) => a - b);
    const lo = Number(round) * 3000;
    const observations = idx.map((i) => {
      const timeMs = Math.max(lo, time - 40 - 7 * i);
      const prices = live.map((m) => ({ marketId: m.id, price: String(this.toWire(m, 'price', m.index)), sources: 5 }));
      const msg = { round: String(round), timeMs: String(timeMs), prices };
      const digest = typedDataHash(this.eip712.domain, 'OracleObservation', ORACLE_TYPES.OracleObservation, msg, ORACLE_TYPES);
      return { signer: i, timeMs: String(timeMs), prices, signature: hex(sign(digest, this.oracleKeys[i])) };
    });
    return { round: String(round), observations };
  }
  blockSignature(b) {
    if (!b._sig) b._sig = hex(sign(keccak256(concat(enc.encode('postern-perps-mock/block'), unhex(b.hash))), this.sequencerKey));
    return b._sig;
  }
  accountLeaves() {
    const out = [];
    for (const a of [...this.accounts.values()].sort((x, y) => (x.address < y.address ? -1 : 1))) {
      const data = { address: a.address, collateral: a.collateral, positions: [...a.positions].filter(([, p]) => p.size !== 0n).map(([id, p]) => ({ marketId: id, size: p.size, cost: p.cost, isolatedMargin: p.isolatedMargin })) };
      out.push({ address: a.address, data, leaf: h32(canonical({ mockLeaf: 'account-v0', ...data })) });
    }
    return out;
  }
  /**
   * The sixteen checkpoint fields of a block (VAULT.md 5.2) and the SPEC 11 state-root equation over them. The
   * subtrees (accounts, markets, withdrawals) are the mock's own Merkle format (README "Fidelity"); the equation, the
   * system hash, the chain heads and the withdrawal count and total are VAULT.md's.
   */
  stateFields(height, time, logHash, leaves) {
    const wds = [];
    const CERT = new Set(['paid', 'delayed', 'submitted', 'certified', 'finalized']);
    for (const a of this.accounts.values()) {
      for (const w of a.withdrawals) {
        if (w.requestedHeight > height) continue;
        if (w.demo ? CERT.has(w.target) : !['requested', 'rejected'].includes(w.status)) wds.push(w);
      }
    }
    wds.sort((x, y) => (x.withdrawalId < y.withdrawalId ? -1 : 1));
    const wTree = merkle(wds.map((w) => (w._leaf ??= this.withdrawalLeaf(w))));
    const marketLeaves = [...this.markets.values()].sort((x, y) => x.id - y.id).map((m) => h32(canonical({ mockLeaf: 'market-v0', marketId: m.id, index: m.index, mark: m.mark, status: m.status })));
    let totalEscape = 0n;
    for (const a of this.accounts.values()) {
      if (a.address === this.liquidity || a.address === FEE_ACCOUNT) continue;
      const g = this.margins(a);
      if (g.equity > 0n) totalEscape += g.equity;
    }
    const f = {
      height: String(height), time: String(time), logHash,
      accountsRoot: merkle(leaves.map((l) => l.leaf)).root, marketsRoot: merkle(marketLeaves).root, withdrawalsRoot: wTree.root,
      nextDepositIndex: String(this.depositsCredited), nextForcedIndex: String(this.forcedProcessed), nextOrderId: String(this.ids.order + 1n),
      totalEscape: String(totalEscape), dust: '0', auxHash: ZERO32,
      depositsHead: this.depositsHead, forcedHead: this.forcedHead,
      withdrawalCount: String(wds.length), withdrawalsTotal: String(wds.reduce((t, w) => t + w.amount, 0n)),
    };
    return { fields: f, stateRoot: stateRootOf(f), wTree, wds };
  }
  checkpoint(b, ev) {
    const { fields, wTree, wds } = b.st;
    const digestBytes = typedDataHash(VAULT_DOMAIN(this.vault), 'Checkpoint', CHECKPOINT_FIELDS, fields);
    // Exactly `threshold` signatures, sorted by validator address ascending; the signing subset rotates by height.
    const k = b.height % this.committee.length;
    const signers = [...this.committee.slice(k), ...this.committee.slice(0, k)].slice(0, this.threshold).sort((x, y) => (x.address < y.address ? -1 : 1));
    const cp = { height: b.height, time: b.time, blockHash: b.hash, stateRoot: b.stateRoot, logHash: b.logHash, fields, withdrawalsRoot: wTree.root, digest: hex(digestBytes), threshold: this.threshold, final: true, signers, _sigs: null, leaves: b.leaves, wTree, wds };
    this.checkpoints.push(cp);
    if (this.checkpoints.length > 500) this.checkpoints.shift();
    for (const [i, w] of wds.entries()) {
      if (w.status === 'included') {
        w.status = 'finalized';
        w.checkpointHeight = b.height;
        w.cert = { cp, index: i };
        ev.withdrawals(this.account(w.account), w);
      }
    }
    ev.checkpoint = cp;
  }
  checkpointSignatures(cp) {
    if (!cp._sigs) cp._sigs = cp.signers.map((s) => ({ validator: s.address, signature: hex(sign(unhex(cp.digest), s.key)) }));
    return cp._sigs;
  }
  withdrawalLeaf(w) {
    return hex(typedDataHash(VAULT_DOMAIN(this.vault), 'WithdrawalRecord', WITHDRAWAL_FIELDS, this.withdrawalRecord(w)));
  }
  withdrawalRecord(w) {
    return { withdrawalId: String(w.withdrawalId), account: w.account, recipient: w.recipient, amount: String(w.amount), fee: String(w.fee), height: String(w.requestedHeight) };
  }
  /**
   * Withdrawal states (API.md 4.1): included -> finalized (in a checkpoint) -> certified (the next checkpoint
   * exists, the certificate is published) -> submitted (the mock relayer, 1 s later, only while the vault is
   * normal) or delayed (>= 100000 USDC, until availableAt) -> paid. Demo withdrawals jump once to their target.
   */
  progressWithdrawals(ev) {
    const latest = this.checkpoints.at(-1);
    for (const acc of this.accounts.values()) {
      for (const w of acc.withdrawals) {
        const before = w.status;
        if (w.demo) {
          if (w.cert && !w.done && w.cert.cp !== latest) {
            w.done = true;
            w.status = w.target;
            if (w.target !== 'finalized') w.certificate = w.cert;
            if (w.target === 'delayed') w.availableAt = this.time + 86_400_000;
            if (w.target === 'submitted' || w.target === 'paid') w.l2TxHash = h32(`l2tx/wd/${w.withdrawalId}`);
          }
        } else if (w.status === 'finalized' && w.cert && w.cert.cp !== latest) {
          w.status = 'certified';
          w.certificate = w.cert;
          w.certifiedAt = this.time;
        } else if (w.status === 'certified' && this.vaultMode === 'normal' && this.time - w.certifiedAt >= 1000) {
          w.l2TxHash = h32(`l2tx/wd/${w.withdrawalId}`);
          if (w.amount >= parseFixed('100000', USDC_DP)) {
            w.status = 'delayed';
            w.availableAt = this.time + 86_400_000;
          } else {
            w.status = 'submitted';
            w.submittedAt = this.time;
          }
        } else if (w.status === 'submitted' && this.time - w.submittedAt >= 2000) {
          w.status = 'paid';
        } else if (w.status === 'delayed' && this.vaultMode === 'normal' && this.time >= w.availableAt) {
          w.status = 'paid';
        }
        if (w.status !== before) ev.withdrawals(acc, w);
      }
    }
  }
  snapshotLevels(m) {
    return { bids: new Map(this.levels(m, 'bids').map(([p, s]) => [p, s])), asks: new Map(this.levels(m, 'asks').map(([p, s]) => [p, s])) };
  }
  /** A simulated taker (the liquidity account) sweeping up to five ticks from the best opposite level. */
  simTaker(m, ev) {
    const isBuy = m.rng.chance(1, 2);
    let size = (m.levelSize * m.rng.range(5n, 120n)) / 100n;
    size -= size % m.step;
    if (size < m.step) size = m.step;
    const ref = this.best(m, isBuy ? 'asks' : 'bids');
    if (ref === null) return;
    const worst = isBuy ? ref + m.tick * 5n : ref - m.tick * 5n;
    const o = { orderId: 0n, isBuy, size, filled: 0n, filledCost: 0n, m, sim: true };
    this.match(m, this.account(this.liquidity), o, worst, ev);
  }
  updateMark(m) {
    const bb = this.best(m, 'bids'), ba = this.best(m, 'asks');
    const vals = [m.index, bb ?? m.index, ba ?? m.index].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    const cap = (m.index * m.bandBps) / 10_000n; // S-5.2: the book component is bounded to markBandBps
    let mark = vals[1];
    if (mark > m.index + cap) mark = m.index + cap;
    if (mark < m.index - cap) mark = m.index - cap;
    m.mark = this.roundTick(m, mark);
    m.premiumSum += ((m.mark - m.index) * pow10(RATE_DP)) / m.index;
    m.premiumN += 1n;
  }
  triggers(m, ev) {
    for (const acc of this.accounts.values()) {
      for (const o of [...acc.orders.values()]) {
        if (o.m !== m || o.status !== 'untriggered') continue;
        const hit = o.triggerCondition === 'markAbove' ? m.mark >= o.triggerPrice : m.mark <= o.triggerPrice;
        if (!hit) continue;
        acc.orders.delete(o.orderId);
        const p = this.position(acc, m);
        if (o.reduceOnly) {
          if (p.size === 0n || (p.size > 0n) === o.isBuy) {
            o.status = 'canceled';
            o.updatedAt = this.time;
            acc.history.unshift(o);
            ev.orders(acc, o);
            continue;
          }
          o.size = min(o.size, abs(p.size));
        }
        try {
          this.execute(acc, m, o, ev);
        } catch (e) {
          if (!(e instanceof EngineError)) throw e;
        }
      }
    }
  }
  liquidations(ev) {
    for (const acc of this.accounts.values()) {
      if (acc.address === this.liquidity || acc.address === FEE_ACCOUNT) continue;
      let open = false;
      for (const p of acc.positions.values()) if (p.size !== 0n) open = true;
      if (!open) continue;
      const g = this.margins(acc);
      if (g.equity >= g.mm) continue;
      this.liquidate(acc, null, ev);
    }
  }
  /** Close an account's positions (all, or one market) at the mark against the backstop (the liquidity account). */
  liquidate(acc, market, ev) {
    for (const [id, p] of acc.positions) {
      if (p.size === 0n) continue;
      const m = this.byId.get(id);
      if (market && m !== market) continue;
      for (const o of [...acc.orders.values()]) if (o.m === m) this.close(acc, o, 'canceled', ev);
      const size = -p.size;
      const { trade } = this.settleFill(m, acc, this.account(this.liquidity), size, m.mark, { ev, liquidation: true });
      ev.trade(m, trade);
    }
    if (acc.collateral < 0n) {
      // Bad debt: the insurance fund absorbs it (the mock lets insurance go negative and shows it in the invariant).
      this.insurance += acc.collateral;
      acc.collateral = 0n;
    }
    ev.account(acc);
  }
  fundingTick(ev) {
    for (const m of this.markets.values()) {
      if (BigInt(this.time) < m.nextFunding) continue;
      const premium = m.premiumN ? m.premiumSum / m.premiumN : 0n;
      const interest = m.interest;
      let rate = premium + interest;
      if (rate > m.maxFunding) rate = m.maxFunding;
      if (rate < -m.maxFunding) rate = -m.maxFunding;
      m.fundingRate = rate;
      m.funding.unshift({ symbol: m.symbol, time: Number(m.nextFunding), rate, premium, mark: m.mark, index: m.index });
      // Longs pay shorts when the rate is positive: payers are charged rounded up, receivers credited rounded
      // down, the difference goes to the insurance fund, so funding conserves value exactly.
      let paid = 0n, received = 0n;
      for (const acc of this.accounts.values()) {
        const p = acc.positions.get(m.id);
        if (!p || p.size === 0n) continue;
        const raw = this.notional(m, p.size, m.mark) * abs(rate);
        const pays = (p.size > 0n) === (rate > 0n);
        const amount = pays ? divCeil(raw, pow10(RATE_DP)) : divFloor(raw, pow10(RATE_DP));
        const payment = pays ? -amount : amount;
        acc.collateral += payment;
        p.funding += payment;
        if (pays) paid += amount;
        else received += amount;
        if (acc.address !== this.liquidity) {
          const fp = { symbol: m.symbol, time: Number(m.nextFunding), rate, positionSize: p.size, markPrice: m.mark, payment, m };
          acc.funding.unshift(fp);
          ev.funding(acc, fp);
          ev.account(acc);
        }
      }
      this.insurance += paid - received;
      m.premiumSum = 0n;
      m.premiumN = 0n;
      m.nextFunding += 3_600_000n;
      ev.tickerNow(m);
    }
  }
  candleTick(m, ev) {
    for (const [iv, ms] of Object.entries(INTERVALS)) {
      const list = m.candles[iv];
      const t = Math.floor(this.time / ms) * ms;
      let c = list.at(-1);
      if (c.t !== t) {
        list.push((c = { t, o: c.c, h: c.c, l: c.c, c: c.c, v: 0n, q: 0n, n: 0 }));
        if (list.length > HISTORY_CANDLES + 2000) list.shift();
        ev.candle(m, iv, list.at(-2));
      }
      for (const tr of ev.tradesOf(m)) {
        c.h = max(c.h, tr.price);
        c.l = min(c.l, tr.price);
        c.c = tr.price;
        c.v += tr.size;
        c.q += this.notional(m, tr.size, tr.price);
        c.n += 1;
      }
      ev.candle(m, iv, c);
    }
  }

  /** Event collector for one block (the WebSocket layer turns it into messages). */
  newEvents() {
    const e = {
      trades: new Map(), deltas: new Map(), candles: new Map(), accounts: new Map(), tickers: new Set(), checkpoint: null, block: null,
      acc: (a) => {
        let x = e.accounts.get(a.address);
        if (!x) e.accounts.set(a.address, (x = { orders: new Map(), fills: [], funding: [], deposits: new Map(), withdrawals: new Map(), actions: [], account: false, agents: false }));
        return x;
      },
      trade: (m, t) => {
        if (!e.trades.has(m)) e.trades.set(m, []);
        e.trades.get(m).push(t);
        m.trades.unshift(t);
        if (m.trades.length > 5000) m.trades.length = 5000;
      },
      tradesOf: (m) => e.trades.get(m) ?? [],
      orders: (a, o) => e.acc(a).orders.set(o.orderId, o),
      fills: (a, f) => { e.acc(a).fills.push(f); e.acc(a).account = true; },
      funding: (a, f) => e.acc(a).funding.push(f),
      deposits: (a, d) => e.acc(a).deposits.set(d.depositId, d),
      withdrawals: (a, w) => e.acc(a).withdrawals.set(w.withdrawalId, w),
      agents: (a) => (e.acc(a).agents = true),
      account: (a) => (e.acc(a).account = true),
      ack: (a, k) => e.acc(a).actions.push(k),
      book: () => {},
      bookDelta: (m, before, after) => {
        const d = { bids: [], asks: [] };
        for (const side of ['bids', 'asks']) {
          for (const [p, s] of after[side]) if (before[side].get(p) !== s) d[side].push([p, s]);
          for (const p of before[side].keys()) if (!after[side].has(p)) d[side].push([p, 0n]);
          d[side].sort((a, b) => (side === 'bids' ? (a[0] > b[0] ? -1 : 1) : a[0] < b[0] ? -1 : 1));
        }
        if (d.bids.length || d.asks.length) {
          const prevSeq = m.seq;
          m.seq += 1;
          e.deltas.set(m, { ...d, seq: m.seq, prevSeq });
        }
      },
      candle: (m, iv, c) => {
        const k = `${m.symbol}|${iv}`;
        if (!e.candles.has(k)) e.candles.set(k, []);
        const l = e.candles.get(k);
        if (!l.includes(c)) l.push(c);
      },
      tickerNow: (m) => e.tickers.add(m),
    };
    return e;
  }

  /** Conservation: collateral + isolated margins + fee account + insurance - signed cost basis == net deposits. */
  conservation() {
    let lhs = this.insurance;
    for (const a of this.accounts.values()) {
      lhs += a.collateral;
      for (const [id, p] of a.positions) {
        lhs += p.isolatedMargin;
        lhs -= this.usdc(this.byId.get(id), this.signedCost(p));
      }
    }
    let sizes = new Map();
    for (const a of this.accounts.values()) for (const [id, p] of a.positions) sizes.set(id, (sizes.get(id) ?? 0n) + p.size);
    return { lhs, rhs: this.netDeposits, ok: lhs === this.netDeposits, netSizes: [...sizes].map(([id, s]) => [id, s]), sizesZero: [...sizes.values()].every((s) => s === 0n) };
  }

  /** Mock deposit seen on the L2 (scenario): seen -> l2Finalized -> credited over the next blocks. */
  mockDeposit(address, amountText) {
    const acc = this.account(address);
    acc.seeded ||= !this.autoDemo ? false : acc.seeded;
    const id = ++this.ids.deposit;
    const d = { depositId: id, account: acc.address, amount: parseFixed(amountText, USDC_DP), l2TxHash: h32(`l2tx/dep/${acc.address}/${id}`), logIndex: 0, l2Block: ++this.l2.lastDepositBlock, status: 'seen', creditedHeight: null, time: this.time, finalAt: this.time + 5 * this.blockMs, demo: false };
    this.l2.finalizedBlock = this.l2.lastDepositBlock;
    acc.deposits.unshift(d);
    return d;
  }
}

export { USDC_DP, RATE_DP, parseU64 };
