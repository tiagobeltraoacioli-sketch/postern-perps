// Exact integer helpers of the rehearsal (SPEC scales: price and size 1e-8, USDC 1e-6). UNAUDITED prototype.
// Copyright (c) 2026 Bloch Inc. All rights reserved.
import { usdc, px, sz } from '../lib/client.mjs';

export const SYSTEM = { fee: '0x0000000000000000000000000000000000000001', insurance: '0x0000000000000000000000000000000000000002', backstop: '0x0000000000000000000000000000000000000003' };
export const OWNER_FEE_ADDRESS = '0x775a993f09bd80aecf8f8c2ade0c84158e3a0544';
const ceilDiv = (a, b) => (a + b - 1n) / b;

/** SPEC S-9.3: fee = ceil(|size| * price * feePpm / 10^6 / 10^10) USDC units. */
export const tradeFee = (price, size, ppm) => ceilDiv(px(price) * sz(size) * BigInt(ppm), 10n ** 16n);
/** notional in USDC units, exact when it divides (the rehearsal uses round numbers). */
export const notionalUsdc = (price, size) => (px(price) * sz(size)) / 10n ** 10n;
export const fmtUsdc = (v) => { const n = v < 0n ? -v : v; return `${v < 0n ? '-' : ''}${n / 1000000n}.${String(n % 1000000n).padStart(6, '0')}`; };
export const bal = async (api, address) => usdc((await api.account(address)).collateral);
/** A price on the market's tick at `bps` basis points from `ref` (decimal string in, decimal string out). */
export function onTick(m, ref, bps) {
  const tick = px(m.tickSize);
  const p = (px(ref) * BigInt(10000 + bps)) / 10000n;
  const t = (p / tick) * tick;
  const s = String(t).padStart(9, '0');
  return `${s.slice(0, -8)}.${s.slice(-8)}`.replace(/\.?0+$/, '') || '0';
}
/** Sum of the collateral of every account the rehearsal knows plus the three system accounts (USDC units). */
export async function totalBalances(api, CAST) {
  let t = 0n;
  const per = {};
  for (const a of [...CAST.map((c) => [c.label, c.address]), ...Object.entries(SYSTEM)]) { per[a[0]] = await bal(api, a[1]); t += per[a[0]]; }
  return { total: t, per: Object.fromEntries(Object.entries(per).map(([k, v]) => [k, fmtUsdc(v)])) };
}

/** Sum of the deposits the ENGINE credited, read from the API for every rehearsal account and the system accounts
 *  (never from the rehearsal's own memory, so a phase run alone still checks against what really happened). */
export async function creditedDeposits(api, CAST) {
  let t = 0n;
  for (const a of [...CAST.map((c) => c.address), ...Object.values(SYSTEM)]) {
    for (const d of (await api.json(`/v1/accounts/${a}/deposits`)).deposits) if (d.status === 'credited') t += usdc(d.amount);
  }
  return t;
}
/** Sum of every withdrawal RECORD (the net amounts that leave the engine: SPEC I-2 counts records; the flat fee of
 *  a withdrawal never leaves, it moves to the fee account's balance). */
export async function withdrawalRecords(api, CAST) {
  let t = 0n;
  for (const a of [...CAST.map((c) => c.address), ...Object.values(SYSTEM)]) {
    for (const w of (await api.json(`/v1/accounts/${a}/withdrawals`)).withdrawals) if (w.status !== 'rejected') t += usdc(w.amount);
  }
  return t;
}
/** Exact funding payment of SPEC S-7.3 in USDC units for a position (decimal strings from the API):
 *  f = size * mark * rate / 1e9 (notional units) / 1e10; a payer's amount is rounded UP, a receiver's DOWN. */
export function fundingPayment(size, mark, rate) {
  const n = sz(size) * px(mark) * scale9(rate);          // signed: size and rate carry the signs
  const d = 10n ** 19n;
  const pays = n > 0n;                                    // long with positive rate, or short with negative rate
  const abs = n < 0n ? -n : n;
  return pays ? -((abs + d - 1n) / d) : abs / d;
}
function scale9(s) {
  const neg = s.startsWith('-');
  const [i, f = ''] = (neg ? s.slice(1) : s).split('.');
  if (f.length > 9) throw new Error(`rate ${s} has more than 9 decimals`);
  const v = BigInt(i + f.padEnd(9, '0'));
  return neg ? -v : v;
}
