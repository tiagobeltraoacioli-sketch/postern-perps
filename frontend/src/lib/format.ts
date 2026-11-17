/**
 * Number formatting for display. Everything is formatted from fixed-point integers with string arithmetic, so a
 * shown figure is the exact value cut or rounded as stated, never a float artefact. The minus sign is U+2212 and
 * digits are meant for `font-variant-numeric: tabular-nums` columns.
 */
import { PX_DECIMALS, SZ_DECIMALS, USDC_DECIMALS, RATE_DECIMALS, abs, mulDiv } from './fixed';

export const MINUS = '−';
export const DASH = '—';

function group(whole: string): string {
  return whole.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
}

export type FormatOptions = {
  /** Decimal places shown. */
  dp: number;
  /** 'always' prefixes + on positive values. */
  sign?: 'auto' | 'always' | 'never';
  /** How the value is cut to `dp` places. Default half (away from zero). */
  rounding?: 'floor' | 'half';
  grouping?: boolean;
  /** Drop trailing zeros after the point, down to `minDp`. */
  trim?: boolean;
  minDp?: number;
};

/** Formats a fixed-point integer of `decimals` places. */
export function formatFixed(value: bigint, decimals: number, options: FormatOptions): string {
  const { dp, sign = 'auto', rounding = 'half', grouping = true, trim = false, minDp = 0 } = options;
  const neg = value < 0n;
  let a = abs(value);
  if (dp < decimals) {
    const cut = 10n ** BigInt(decimals - dp);
    a = rounding === 'half' ? (a + cut / 2n) / cut : a / cut;
  } else if (dp > decimals) a *= 10n ** BigInt(dp - decimals);
  const unit = 10n ** BigInt(dp);
  const whole = (a / unit).toString();
  let frac = dp > 0 ? (a % unit).toString().padStart(dp, '0') : '';
  if (trim) {
    frac = frac.replace(/0+$/, '');
    if (frac.length < minDp) frac = frac.padEnd(minDp, '0');
  }
  const body = `${grouping ? group(whole) : whole}${frac ? `.${frac}` : ''}`;
  const zero = a === 0n;
  const prefix = sign === 'never' || zero ? '' : neg ? MINUS : sign === 'always' ? '+' : '';
  return prefix + body;
}

/** A price at the market's price decimals. */
export const formatPx = (px: bigint, dp: number) => formatFixed(px, PX_DECIMALS, { dp });
/** A size at the market's size decimals. */
export const formatSize = (size: bigint, dp: number, sign: FormatOptions['sign'] = 'never') => formatFixed(size, SZ_DECIMALS, { dp, sign });
/** USDC with two places (cut toward zero by default for balances; half for PnL). */
export const formatUsd = (value: bigint, options: Partial<FormatOptions> = {}) => formatFixed(value, USDC_DECIMALS, { dp: 2, ...options });

/** Compact dollars for stats: 1.24B, 318.5M, 12.4K, 950. */
export function formatCompactUsd(value: bigint): string {
  const a = abs(value);
  const units: [bigint, string][] = [
    [1_000_000_000_000_000n, 'B'],
    [1_000_000_000_000n, 'M'],
    [1_000_000_000n, 'K'],
  ];
  for (const [unit, suffix] of units)
    if (a >= unit) {
      const scaled = mulDiv(a, 100n, unit, 'half');
      const dp = scaled >= 10_000n ? 1 : 2;
      return `${value < 0n ? MINUS : ''}${formatFixed(scaled, 2, { dp })}${suffix}`;
    }
  return formatFixed(value, USDC_DECIMALS, { dp: 0 });
}

/** Parts per million as a percentage: 12_345 -> "1.23%". */
export function formatPpm(ppm: bigint, dp = 2, sign: FormatOptions['sign'] = 'auto'): string {
  return `${formatFixed(ppm, 4, { dp, sign })}%`;
}
/** Parts per million as basis points: 450 -> "4.5 bps". */
export function formatBps(ppm: bigint): string {
  return `${formatFixed(ppm, 2, { dp: 2, trim: true })} bps`;
}
/** A funding rate (rate scale) as a percentage with four places: 10_000 -> "0.0100%". */
export function formatRate(rate: bigint, sign: FormatOptions['sign'] = 'always'): string {
  return `${formatFixed(rate, RATE_DECIMALS - 2, { dp: 4, sign })}%`;
}

export function shortAddress(address: string): string {
  return address.length > 12 ? `${address.slice(0, 6)}…${address.slice(-4)}` : address;
}

const two = (n: number) => String(n).padStart(2, '0');
/** HH:MM:SS in UTC. */
export function formatClock(ms: number): string {
  const d = new Date(ms);
  return `${two(d.getUTCHours())}:${two(d.getUTCMinutes())}:${two(d.getUTCSeconds())}`;
}
/** YYYY-MM-DD HH:MM UTC. */
export function formatDateTime(ms: number): string {
  const d = new Date(ms);
  return `${d.getUTCFullYear()}-${two(d.getUTCMonth() + 1)}-${two(d.getUTCDate())} ${two(d.getUTCHours())}:${two(d.getUTCMinutes())}`;
}
/** A countdown: 59:07, or 1:02:03 above an hour. Negative durations show 0:00. */
export function formatCountdown(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h > 0 ? `${h}:${two(m)}:${two(s % 60)}` : `${m}:${two(s % 60)}`;
}
/** "3 min", "2 h 5 min", "1 d 4 h": a duration in words for explanations. */
export function formatDuration(ms: number): string {
  const m = Math.max(0, Math.round(ms / 60_000));
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} d ${h % 24} h` : `${d} d`;
}
