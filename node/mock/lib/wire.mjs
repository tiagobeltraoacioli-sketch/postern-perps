// JSON shapes of docs/API.md v0.1 from the mock exchange's integer state. UNAUDITED prototype.
// Every number leaves as a decimal string in human units (API.md 1); 64-bit ids as decimal strings.
import { formatFixed, pow10, divFloor, divCeil, abs } from './fixed.mjs';
import { INTERVALS, FEE_ACCOUNT, merkle } from './exchange.mjs';
import { tree32 } from './merkle32.mjs';

const U = 6;
const RATE_DP = 10;
const usd = (v) => formatFixed(v, U);
const rate = (v) => trimRate(formatFixed(v, RATE_DP));
function trimRate(t) {
  return t.includes('.') ? t.replace(/0+$/, '').replace(/\.$/, '.0') : t;
}
const px = (m, v) => formatFixed(v, m.pd);
const sz = (m, v) => formatFixed(v, m.sd);

export function market(x, m) {
  const c = m.cfg;
  const out = {
    symbol: m.symbol, marketId: m.id, base: c.base, quote: c.quote ?? 'USD', status: m.status,
    priceDecimals: m.pd, sizeDecimals: m.sd, tickSize: c.tickSize, stepSize: c.stepSize,
    minOrderSize: c.minOrderSize, maxOrderSize: c.maxOrderSize, maxLeverage: c.maxLeverage,
    marginTiers: c.marginTiers.map((t) => ({ notionalFloor: t.notionalFloor, initialMarginFraction: t.initialMarginFraction, maintenanceMarginFraction: t.maintenanceMarginFraction, maxLeverage: t.maxLeverage })),
    makerFeeBps: c.makerFeeBps, takerFeeBps: c.takerFeeBps, liquidationFeeBps: c.liquidationFeeBps,
    fundingIntervalSec: c.fundingIntervalSec, maxFundingRate: c.maxFundingRate,
    oracle: { sources: c.oracle.sources, minSources: c.oracle.minSources, maxStalenessMs: c.oracle.maxStalenessMs, maxDeviationBps: c.oracle.maxDeviationBps },
    openInterestCap: c.openInterestCap,
  };
  if (m.haltReason) out.haltReason = m.haltReason;
  return out;
}

/** 24 h statistics from the 1m candles (history plus live). */
function stats24(x, m) {
  const list = m.candles['1m'];
  const since = x.time - 86_400_000;
  let hi = null, lo = null, vol = 0n, open = null, oi = 0n;
  for (let i = list.length - 1; i >= 0 && list[i].t >= since; i--) {
    const c = list[i];
    hi = hi === null || c.h > hi ? c.h : hi;
    lo = lo === null || c.l < lo ? c.l : lo;
    vol += c.q;
    open = c.o;
  }
  for (const a of x.accounts.values()) {
    const p = a.positions.get(m.id);
    if (p && p.size > 0n) oi += p.size;
  }
  return { hi: hi ?? m.last, lo: lo ?? m.last, vol, open: open ?? m.last, oi };
}

export function ticker(x, m) {
  const s = stats24(x, m);
  // change24h = (last - open) / open as a fraction with 6 decimals, rounded toward zero.
  const ch = ((m.last - s.open) * pow10(6)) / s.open;
  return {
    symbol: m.symbol, markPrice: px(m, m.mark), indexPrice: px(m, m.index), lastPrice: px(m, m.last),
    fundingRate: rate(currentFunding(m)), nextFundingTime: Number(m.nextFunding),
    openInterest: sz(m, s.oi), volume24h: usd(s.vol), high24h: px(m, s.hi), low24h: px(m, s.lo),
    price24hAgo: px(m, s.open), change24h: formatFixed(ch, 6), height: x.height, time: x.time,
  };
}
function currentFunding(m) {
  const premium = m.premiumN ? m.premiumSum / m.premiumN : 0n;
  let r = premium + m.interest;
  if (r > m.maxFunding) r = m.maxFunding;
  if (r < -m.maxFunding) r = -m.maxFunding;
  return r;
}

export function book(x, m, depth) {
  return {
    symbol: m.symbol, seq: m.seq, height: x.height, time: x.time,
    bids: x.levels(m, 'bids', depth).map(([p, s]) => [px(m, p), sz(m, s)]),
    asks: x.levels(m, 'asks', depth).map(([p, s]) => [px(m, p), sz(m, s)]),
  };
}
export function bookDelta(x, m, d) {
  return { symbol: m.symbol, seq: d.seq, prevSeq: d.prevSeq, height: x.height, time: x.time, bids: d.bids.map(([p, s]) => [px(m, p), sz(m, s)]), asks: d.asks.map(([p, s]) => [px(m, p), sz(m, s)]) };
}
export const trade = (t) => ({ tradeId: String(t.tradeId), symbol: t.m.symbol, price: px(t.m, t.price), size: sz(t.m, t.size), side: t.isBuy ? 'buy' : 'sell', time: t.time, height: t.height, liquidation: t.liquidation });
export const candle = (m, c) => ({ t: c.t, o: px(m, c.o), h: px(m, c.h), l: px(m, c.l), c: px(m, c.c), v: sz(m, c.v), q: usd(c.q), n: c.n });
export const fundingRate = (m, f) => ({ symbol: m.symbol, time: f.time, rate: rate(f.rate), premium: rate(f.premium), markPrice: px(m, f.mark), indexPrice: px(m, f.index) });

export function oracle(x, m) {
  const r = m.rng.fork(`oracle/${x.oracleRound}`);
  const status = m.oracleState === 'ok' ? 'ok' : m.oracleState;
  const sources = m.cfg.oracle.sources.map((name, i) => {
    const off = (m.index * r.range(-15n, 15n)) / 100_000n;
    const outlier = status === 'deviation' && i === 0;
    const stale = status === 'stale';
    const price = outlier ? (m.index * 97n) / 100n : m.index + off;
    return { name, price: px(m, x.roundTick(m, price)), time: x.time - (stale ? 60_000 : Number(r.range(100n, 900n))), used: status === 'ok' && !outlier, ...(outlier ? { reason: 'outlier' } : stale ? { reason: 'stale' } : {}) };
  });
  return { symbol: m.symbol, status, price: px(m, m.index), time: Number(x.oracleRound) * 3000, round: String(x.oracleRound), sources, signers: status === 'ok' ? 5 : 2, threshold: 5, haltReason: m.status === 'halted' ? m.haltReason : null };
}

export const blockRef = (b) => (b ? { height: b.height, time: b.time, hash: b.hash, stateRoot: b.stateRoot, logHash: b.logHash } : null);
export function status(x, mode, pendingOverride) {
  const latest = x.blocks.get(x.height);
  const cp = x.checkpoints.at(-1);
  return {
    name: 'postern-perps-mock', version: '0.1.0', apiVersion: '0.1', mode, unaudited: true,
    network: { eip712: { ...x.eip712.domain }, collateralChainId: 700770, vault: x.vault, collateralToken: x.collateralToken, collateralSymbol: 'bUSDC', collateralDecimals: 6 },
    time: x.time,
    latestBlock: blockRef(latest) ?? { height: x.height, time: x.time, hash: x.genesisLogHash, stateRoot: x.genesisLogHash, logHash: x.lastLogHash },
    latestFinalized: cp ? { height: cp.height, time: cp.time, hash: cp.blockHash, stateRoot: cp.stateRoot, logHash: cp.logHash } : null,
    sequencer: { state: x.sequencerState, blockIntervalMs: x.blockMs, pendingActions: pendingOverride ?? x.pending.length },
    committee: { n: x.committee.length, threshold: x.threshold, online: x.finalityStalled ? x.threshold - 1 : x.committee.length },
    vaultMode: x.vaultMode, l2: { finalizedBlock: x.l2.finalizedBlock, lastDepositBlock: x.l2.lastDepositBlock, endpointsAgree: true },
  };
}
export function checkpoint(x, cp) {
  // VAULT.md 5.2 fields with the node's JSON types (389 b170e74 checkpoint.rs): u64 counters that fit 2^53 as
  // numbers, nextOrderId / totalEscape / dust / withdrawalsTotal as decimal strings.
  const f = cp.fields;
  return {
    height: cp.height, time: cp.time, blockHash: cp.blockHash, stateRoot: cp.stateRoot, logHash: f.logHash,
    accountsRoot: f.accountsRoot, marketsRoot: f.marketsRoot, withdrawalsRoot: f.withdrawalsRoot,
    nextDepositIndex: Number(f.nextDepositIndex), nextForcedIndex: Number(f.nextForcedIndex), nextOrderId: f.nextOrderId,
    totalEscape: f.totalEscape, dust: f.dust, auxHash: f.auxHash, depositsHead: f.depositsHead, forcedHead: f.forcedHead,
    withdrawalCount: Number(f.withdrawalCount), withdrawalsTotal: f.withdrawalsTotal,
    digest: cp.digest, threshold: cp.threshold, final: cp.final, signatures: x.checkpointSignatures(cp),
  };
}
export function block(x, b) {
  return {
    height: b.height, time: b.time, prevLogHash: b.prevLogHash, logHash: b.logHash, stateRoot: b.stateRoot,
    inputs: b.inputs, inputsEncoded: b.inputsEncoded, sequencerSignature: x.blockSignature(b),
  };
}

export function position(x, acc, id, p) {
  const m = x.byId.get(id);
  const n = x.notional(m, p.size, m.mark);
  const t = x.tier(m, n);
  const imf = t.imf > divCeil(pow10(6), x.leverageOf(acc, m)) ? t.imf : divCeil(pow10(6), x.leverageOf(acc, m));
  const entry = p.size === 0n ? 0n : divFloor(p.cost, abs(p.size)); // entry price rounded down
  const liq = x.liquidationPrice(acc, m, p);
  return {
    symbol: m.symbol, size: sz(m, p.size), entryPrice: px(m, entry), markPrice: px(m, m.mark), notional: usd(n),
    unrealizedPnl: usd(x.upnl(m, p)), realizedPnl: usd(p.realized), liquidationPrice: liq === null ? null : px(m, liq),
    marginMode: p.mode, leverage: String(x.leverageOf(acc, m)), isolatedMargin: p.mode === 'isolated' ? usd(p.isolatedMargin) : null,
    initialMargin: usd(divCeil(n * imf, pow10(6))), maintenanceMargin: usd(divCeil(n * t.mmf, pow10(6))), fundingSinceOpen: usd(p.funding),
  };
}
export function account(x, acc) {
  const g = x.margins(acc);
  const positions = [...acc.positions].filter(([, p]) => p.size !== 0n).sort((a, b) => a[0] - b[0]).map(([id, p]) => position(x, acc, id, p));
  // marginRatio = maintenance / equity with 6 decimals, rounded up; an account with positions and equity <= 0
  // reports 999999 (liquidatable), one without positions reports 0.
  const ratio = g.mm === 0n ? 0n : g.equity <= 0n ? 999_999_000_000n : divCeil(g.mm * pow10(6), g.equity);
  return {
    address: acc.address, height: x.height, time: x.time,
    collateral: usd(acc.collateral), unrealizedPnl: usd(g.upnl), equity: usd(g.equity),
    initialMargin: usd(g.im), maintenanceMargin: usd(g.mm), freeCollateral: usd(g.equity - g.im),
    withdrawable: usd(x.withdrawable(acc)), marginRatio: formatFixed(ratio, 6),
    feeTier: { tier: 0, makerFeeBps: x.byId.get(1)?.cfg.makerFeeBps ?? '1.5', takerFeeBps: x.byId.get(1)?.cfg.takerFeeBps ?? '4.5', volume30d: usd(acc.volume30d) },
    positions, openOrders: [...acc.orders.values()].length,
  };
}
export function order(o) {
  const m = o.m;
  return {
    orderId: String(o.orderId), clientOrderId: o.clientOrderId ?? null, account: o.account, symbol: m.symbol,
    side: o.isBuy ? 'buy' : 'sell', type: o.type, timeInForce: o.tif, price: px(m, o.price), size: sz(m, o.size),
    filledSize: sz(m, o.filled), avgFillPrice: o.filled > 0n ? px(m, divFloor(o.filledCost, o.filled)) : null,
    reduceOnly: o.reduceOnly, triggerPrice: o.triggerPrice === null ? null : px(m, o.triggerPrice), triggerCondition: o.triggerCondition ?? null,
    status: o.status, createdAt: o.createdAt, updatedAt: o.updatedAt, height: o.height, rejectReason: o.rejectReason,
  };
}
export const fill = (f) => ({ fillId: String(f.fillId), tradeId: String(f.tradeId), orderId: String(f.orderId), account: f.account, symbol: f.m.symbol, side: f.isBuy ? 'buy' : 'sell', price: px(f.m, f.price), size: sz(f.m, f.size), fee: usd(f.fee), liquidity: f.liquidity, realizedPnl: usd(f.realizedPnl), liquidation: f.liquidation, time: f.time, height: f.height });
export const fundingPayment = (f) => ({ symbol: f.symbol, time: f.time, rate: rate(f.rate), positionSize: sz(f.m, f.positionSize), markPrice: px(f.m, f.markPrice), payment: usd(f.payment) });
export const agent = (a) => ({ agent: a.agent, name: a.name, approvedAt: a.approvedAt, expiresAt: a.expiresAt, height: a.height });
export const deposit = (d) => ({ depositId: String(d.depositId), account: d.account, amount: usd(d.amount), l2TxHash: d.l2TxHash, logIndex: d.logIndex, l2Block: d.l2Block, status: d.status, creditedHeight: d.creditedHeight, time: d.time });
export function withdrawal(x, w) {
  let certificate = null;
  if (w.certificate) {
    const { cp, index } = w.certificate;
    certificate = { digest: cp.wTree ? x.withdrawalLeaf(w) : null, record: x.withdrawalRecord(w), checkpoint: checkpoint(x, cp), proof: cp.wTree.proof(index), signatures: x.checkpointSignatures(cp) };
  }
  return { withdrawalId: String(w.withdrawalId), account: w.account, recipient: w.recipient, amount: usd(w.amount), fee: usd(w.fee), status: w.status, requestedHeight: w.requestedHeight, checkpointHeight: w.checkpointHeight ?? null, certificate, l2TxHash: w.l2TxHash ?? null, availableAt: w.availableAt ?? null, time: w.time };
}
export function accountProof(x, acc) {
  const cp = x.checkpoints.at(-1);
  if (!cp) return null;
  const i = cp.leaves.findIndex((l) => l.address === acc.address);
  if (i < 0) return null;
  // API 0.3: leafData = the vault's Account struct fields + the 88 leaf bytes; proof = the 32 siblings (SPEC 11)
  const tree = cp.tree ?? (cp.tree = tree32(new Map(cp.leaves.map((l) => [l.index, unhexLeaf(l.leaf)]))));
  const l = cp.leaves[i];
  return { address: acc.address, checkpoint: checkpoint(x, cp), leaf: l.leaf, leafData: l.data, proof: tree.proof(l.index), index: String(l.index) };
}
const unhexLeaf = (h) => new Uint8Array(Buffer.from(h.slice(2), 'hex'));
export { INTERVALS, FEE_ACCOUNT };
