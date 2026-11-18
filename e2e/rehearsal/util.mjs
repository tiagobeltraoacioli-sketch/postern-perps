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
