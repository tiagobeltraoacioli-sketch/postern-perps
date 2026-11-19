/**
 * Fixed-point integers, as the engine uses them (no floats in anything shown as money). UNAUDITED prototype.
 *
 * Scales (frontend fixture until docs/SPEC.md of agent 350 lands; this file is the single place to change):
 *   price  : 1e8  (a price of 1 USD is 100_000_000)
 *   size   : 1e8  (one contract of the base asset is 100_000_000)
 *   USDC   : 1e6  (the collateral token bUSDC has 6 decimals)
 *   rate   : 1e9  (SPEC 1 RATE_SCALE: a funding rate of 0.01% is 100_000; round 1 used 1e8 before SPEC existed)
 */
export const PX_DECIMALS = 8;
export const SZ_DECIMALS = 8;
export const USDC_DECIMALS = 6;
export const RATE_DECIMALS = 9;
export const PX = 10n ** BigInt(PX_DECIMALS);
export const SZ = 10n ** BigInt(SZ_DECIMALS);
export const USDC = 10n ** BigInt(USDC_DECIMALS);
export const RATE = 10n ** BigInt(RATE_DECIMALS);
/** price x size / NOTIONAL_DIV = USDC units. */
export const NOTIONAL_DIV = (PX * SZ) / USDC;

const pow10 = (n: number) => 10n ** BigInt(n);

/** Parses a plain decimal string ("12.5", "-0.001") into an integer of `decimals` places. Null when not a number
 *  or when it has more places than `decimals` (nothing is rounded silently). */
export function parseFixed(text: string, decimals: number): bigint | null {
  const m = /^(-?)(\d*)(?:\.(\d*))?$/.exec(text.trim());
  if (!m || (m[2] === '' && (m[3] ?? '') === '')) return null;
  const frac = m[3] ?? '';
  if (frac.length > decimals) return null;
  const value = BigInt(m[2] || '0') * pow10(decimals) + BigInt((frac + '0'.repeat(decimals)).slice(0, decimals) || '0');
  return m[1] ? -value : value;
}

/** As parseFixed, but extra places are cut (toward zero). For API values that carry more precision than shown. */
export function parseFixedLoose(text: string, decimals: number): bigint {
  const m = /^(-?)(\d*)(?:\.(\d*))?$/.exec(String(text).trim());
  if (!m) return 0n;
  const frac = ((m[3] ?? '') + '0'.repeat(decimals)).slice(0, decimals);
  const value = BigInt(m[2] || '0') * pow10(decimals) + BigInt(frac || '0');
  return m[1] ? -value : value;
}

/** The exact decimal string of a fixed-point integer, trailing zeros removed. */
export function toDecimalString(value: bigint, decimals: number): string {
  const neg = value < 0n;
  const abs = neg ? -value : value;
  const whole = abs / pow10(decimals);
  const frac = (abs % pow10(decimals)).toString().padStart(decimals, '0').replace(/0+$/, '');
  return `${neg ? '-' : ''}${whole}${frac ? `.${frac}` : ''}`;
}

export type Rounding = 'floor' | 'ceil' | 'half';
/** a x b / d with an explicit rounding of the true quotient (floor and ceil are toward -inf and +inf). */
export function mulDiv(a: bigint, b: bigint, d: bigint, rounding: Rounding = 'floor'): bigint {
  if (d === 0n) throw new RangeError('division by zero');
  let n = a * b;
  let den = d;
  if (den < 0n) {
    n = -n;
    den = -den;
  }
  const q = n / den; // truncates toward zero
  const r = n % den;
  if (r === 0n) return q;
  if (rounding === 'floor') return n < 0n ? q - 1n : q;
  if (rounding === 'ceil') return n < 0n ? q : q + 1n;
  const twice = (r < 0n ? -r : r) * 2n;
  if (twice >= den) return n < 0n ? q - 1n : q + 1n;
  return q;
}

export const abs = (v: bigint) => (v < 0n ? -v : v);
export const min = (a: bigint, b: bigint) => (a < b ? a : b);
export const max = (a: bigint, b: bigint) => (a > b ? a : b);
export const sign = (v: bigint) => (v > 0n ? 1n : v < 0n ? -1n : 0n);

/** Rounds `value` down to a multiple of `step` (toward zero). */
export function floorToStep(value: bigint, step: bigint): bigint {
  if (step <= 0n) return value;
  return value - (value % step);
}
/** Rounds a price to the tick: down for 'floor', up for 'ceil'. Prices are positive. */
export function toTick(px: bigint, tick: bigint, rounding: 'floor' | 'ceil' | 'half'): bigint {
  if (tick <= 0n) return px;
  const r = px % tick;
  if (r === 0n) return px;
  if (rounding === 'floor') return px - r;
  if (rounding === 'ceil') return px - r + tick;
  return r * 2n >= tick ? px - r + tick : px - r;
}
/** A float for drawing only (chart and depth geometry). Never used for a figure shown as money. */
export const toNumber = (value: bigint, decimals: number) => Number(value) / 10 ** decimals;
