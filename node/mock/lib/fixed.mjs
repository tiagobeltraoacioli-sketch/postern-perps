// Fixed-point decimal helpers on BigInt. No floats anywhere in the mock's state (the engine rule: integers only).
// UNAUDITED prototype, mock/test use only.
//
// A value with `dp` decimals is held as the integer value * 10^dp. Rounding is always explicit: `divFloor` rounds
// toward minus infinity, `divCeil` toward plus infinity; the caller states which side the rounding favours.

const DEC = /^(-?)(0|[1-9]\d*)(?:\.(\d+))?$/;

/** Parse a decimal string with at most `dp` decimals into its scaled integer. Throws on anything else. */
export function parseFixed(text, dp) {
  if (typeof text !== 'string') throw new Error('decimal string expected');
  const m = DEC.exec(text);
  if (!m) throw new Error(`not a decimal string: ${JSON.stringify(text).slice(0, 40)}`);
  const frac = m[3] ?? '';
  if (frac.length > dp) {
    // Extra digits are accepted only when they are zeros ("0.50" for dp 1); otherwise the value is not representable.
    if (/[^0]/.test(frac.slice(dp))) throw new Error(`more than ${dp} decimals: ${text}`);
  }
  const v = BigInt(m[2] + frac.slice(0, dp).padEnd(dp, '0'));
  if (m[1] === '-' && v === 0n) throw new Error('negative zero');
  return m[1] === '-' ? -v : v;
}

/** Format a scaled integer with exactly `dp` decimals. */
export function formatFixed(v, dp) {
  const neg = v < 0n;
  const a = neg ? -v : v;
  const s = a.toString().padStart(dp + 1, '0');
  const out = dp === 0 ? s : `${s.slice(0, s.length - dp)}.${s.slice(s.length - dp)}`;
  return neg ? `-${out}` : out;
}

export const pow10 = (n) => 10n ** BigInt(n);

export function divFloor(a, b) {
  if (b <= 0n) throw new Error('divisor must be positive');
  const q = a / b;
  return a % b !== 0n && a < 0n ? q - 1n : q;
}
export function divCeil(a, b) {
  return -divFloor(-a, b);
}
export const abs = (a) => (a < 0n ? -a : a);
export const min = (a, b) => (a < b ? a : b);
export const max = (a, b) => (a > b ? a : b);

/** Rescale from `from` decimals to `to` decimals, rounding by `mode` ('floor' | 'ceil'). */
export function rescale(v, from, to, mode) {
  if (to >= from) return v * pow10(to - from);
  const d = pow10(from - to);
  return mode === 'ceil' ? divCeil(v, d) : divFloor(v, d);
}

/** A strict canonical unsigned integer as a decimal string (64-bit ids, nonces). */
export function parseU64(text) {
  if (typeof text !== 'string' || !/^(0|[1-9]\d*)$/.test(text)) throw new Error('canonical unsigned integer string expected');
  const v = BigInt(text);
  if (v >= 1n << 64n) throw new Error('above 2^64 - 1');
  return v;
}
