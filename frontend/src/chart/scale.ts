/**
 * Geometry of the chart: the time axis (candle slots), the price axis and their ticks. Pure functions, no DOM.
 *
 * Time is laid out in SLOTS: slot s is the candle whose open time is `anchor + s * intervalMs`. A candle missing
 * from the data is an empty slot (nothing drawn), never a flat candle. Slots are fixed for one (symbol, interval)
 * view, so prepending older history does not move what is on screen.
 *
 * The view is `{ spacing, right }`: `spacing` CSS pixels per slot and `right`, the (fractional) slot at the right
 * edge of the plot. x(slot) = width - (right - slot) * spacing, where x is the slot's centre.
 */
import type { ChartInterval } from './types';
import { decimalsOf, type TimeRank } from './format';

export const INTERVAL_MS: Readonly<Record<ChartInterval, number>> = {
  '1m': 60_000,
  '5m': 300_000,
  '15m': 900_000,
  '1h': 3_600_000,
  '4h': 14_400_000,
  '1d': 86_400_000,
};

export const ALL_INTERVALS: readonly ChartInterval[] = ['1m', '5m', '15m', '1h', '4h', '1d'];

/** Limits of the horizontal zoom, CSS px per candle. Below 1 px a slot would hide candles: never drawn that way. */
export const MIN_SPACING = 1;
export const MAX_SPACING = 64;
export const DEFAULT_SPACING = 8;
/** Empty slots kept right of the newest candle in the default view. */
export const RIGHT_MARGIN_SLOTS = 4;
/** At least this many slots of data stay on screen whatever the pan. */
export const MIN_VISIBLE_SLOTS = 5;

export type View = { readonly spacing: number; readonly right: number };

export function slotX(slot: number, view: View, width: number): number {
  return width - (view.right - slot) * view.spacing;
}

/** The fractional slot under x. */
export function xSlot(x: number, view: View, width: number): number {
  return view.right - (width - x) / view.spacing;
}

/** First and last slot whose candle is at least partly visible (inclusive). */
export function visibleSlots(view: View, width: number): [number, number] {
  return [Math.floor(xSlot(0, view, width) - 0.5), Math.ceil(view.right + 0.5)];
}

/**
 * Zoom by `factor` (> 1 widens candles) keeping the slot under `x` where it is, clamped to the spacing limits.
 */
export function zoomAt(view: View, width: number, x: number, factor: number): View {
  const spacing = clamp(view.spacing * factor, MIN_SPACING, MAX_SPACING);
  const pivot = xSlot(x, view, width);
  return { spacing, right: pivot + (width - x) / spacing };
}

/** Clamp the view so that at least MIN_VISIBLE_SLOTS slots of data [first, last] stay on screen. */
export function clampView(view: View, width: number, first: number, last: number): View {
  const spacing = clamp(view.spacing, MIN_SPACING, MAX_SPACING);
  if (!(last >= first)) return { spacing, right: view.right };
  const slotsOnScreen = width / spacing;
  const keep = Math.min(MIN_VISIBLE_SLOTS, last - first + 1);
  const minRight = first + keep - 1;
  const maxRight = last - keep + 1 + slotsOnScreen;
  return { spacing, right: clamp(view.right, minRight, Math.max(minRight, maxRight)) };
}

/** The default view: newest candle near the right edge, RIGHT_MARGIN_SLOTS empty slots after it. */
export function defaultView(last: number, spacing = DEFAULT_SPACING): View {
  return { spacing, right: last + RIGHT_MARGIN_SLOTS };
}

export function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

// ---------------------------------------------------------------- price axis

export type PriceRange = { readonly min: number; readonly max: number };

export type PriceTicks = {
  readonly step: number;
  /** Tick values, ascending, each an integer multiple of `step` (computed as k * step, never by accumulation). */
  readonly values: number[];
  /** Places needed to write every tick exactly. */
  readonly decimals: number;
};

/**
 * Ticks for [min, max] drawn over `heightPx`, at least `minGapPx` apart. Steps are 1, 2, 2.5 or 5 times a power of
 * ten, never finer than the market's last decimal place (10^-priceDecimals) nor than `tickSize`; 2.5 only when it is
 * writable at `priceDecimals`.
 */
export function priceTicks(
  range: PriceRange,
  heightPx: number,
  priceDecimals: number,
  tickSize = 0,
  minGapPx = 44,
): PriceTicks {
  const span = range.max - range.min;
  if (!(span > 0) || !(heightPx > 0) || !Number.isFinite(span)) return { step: 0, values: [], decimals: 0 };
  const floorStep = Math.max(10 ** -Math.max(0, priceDecimals), tickSize > 0 ? tickSize : 0);
  const raw = Math.max((span * minGapPx) / heightPx, floorStep);
  let step = 0;
  let exp = Math.floor(Math.log10(raw)) - 1;
  for (; !step && exp < 400; exp++) {
    for (const m of [1, 2, 2.5, 5]) {
      const s = m * 10 ** exp;
      if (s + 1e-12 * s < raw) continue;
      if (decimalsOf(s) > priceDecimals) continue;
      if (tickSize > 0 && !isMultiple(s, tickSize)) continue;
      step = s;
      break;
    }
  }
  if (!step) return { step: 0, values: [], decimals: 0 };
  const values: number[] = [];
  const k0 = Math.ceil(range.min / step - 1e-9);
  const k1 = Math.floor(range.max / step + 1e-9);
  for (let k = k0; k <= k1 && values.length < 200; k++) values.push(k * step);
  return { step, values, decimals: Math.min(priceDecimals, decimalsOf(step)) };
}

function isMultiple(a: number, b: number): boolean {
  const q = a / b;
  return Math.abs(q - Math.round(q)) < 1e-9 * Math.max(1, q);
}

/**
 * The price range shown: lows and highs of the visible candles plus the mark and index (they belong to the live
 * edge), padded by `pad` of the span on each side. Levels (entry, liquidation, orders) are NOT included: a far
 * level would squash the candles; it is pinned to the axis edge instead (see levels.ts).
 */
export function autoRange(
  lo: number,
  hi: number,
  extras: readonly (number | null | undefined)[],
  pad = 0.08,
): PriceRange | null {
  let min = lo;
  let max = hi;
  for (const e of extras) {
    if (e === null || e === undefined || !Number.isFinite(e)) continue;
    if (!(min <= e)) min = e;
    if (!(max >= e)) max = e;
  }
  if (!Number.isFinite(min) || !Number.isFinite(max)) return null;
  let span = max - min;
  if (span <= 0) span = Math.abs(max) * 0.002 || 1; // a single flat price still gets a readable axis
  return { min: min - span * pad, max: max + span * pad };
}

export function priceY(price: number, range: PriceRange, top: number, height: number): number {
  return top + ((range.max - price) / (range.max - range.min)) * height;
}

export function yPrice(y: number, range: PriceRange, top: number, height: number): number {
  return range.max - ((y - top) / height) * (range.max - range.min);
}

/** `price` rounded to the nearest multiple of `tick` (no tick: to the market's decimals). */
export function snapPrice(price: number, tickSize: number | undefined, priceDecimals: number): number {
  const tick = tickSize && tickSize > 0 ? tickSize : 10 ** -Math.max(0, priceDecimals);
  const d = Math.max(decimalsOf(tick), 0);
  return Number((Math.round(price / tick) * tick).toFixed(Math.min(20, d)));
}

// ---------------------------------------------------------------- time axis

export type TimeTick = { readonly t: number; readonly rank: TimeRank };

const MIN = 60_000;
const HOUR = 3_600_000;
const DAY = 86_400_000;
/** Fixed tick steps (ms), then calendar months (1, 3, 6) and years. */
const FIXED_STEPS = [MIN, 5 * MIN, 15 * MIN, 30 * MIN, HOUR, 2 * HOUR, 3 * HOUR, 6 * HOUR, 12 * HOUR, DAY, 2 * DAY, 7 * DAY];
const MONTH_STEPS = [1, 3, 6, 12];

function rankOf(t: number): TimeRank {
  const d = new Date(t);
  if (d.getUTCHours() || d.getUTCMinutes() || d.getUTCSeconds() || d.getUTCMilliseconds()) return 'time';
  if (d.getUTCDate() !== 1) return 'day';
  return d.getUTCMonth() === 0 ? 'year' : 'month';
}

/**
 * Time ticks between t0 and t1 (ms, inclusive) for `pxPerMs` pixels per millisecond, at least `minGapPx` apart,
 * aligned to UTC boundaries. Each tick carries its rank so that a day, month or year boundary is labelled as such.
 * Weekly ticks are aligned to Mondays.
 */
export function timeTicks(t0: number, t1: number, pxPerMs: number, minGapPx = 84): TimeTick[] {
  if (!(t1 > t0) || !(pxPerMs > 0)) return [];
  const minStep = minGapPx / pxPerMs;
  const fixed = FIXED_STEPS.find((s) => s >= minStep);
  const out: TimeTick[] = [];
  if (fixed !== undefined && fixed <= 7 * DAY) {
    // Weekly steps: Mondays (1970-01-05 was a Monday).
    const offset = fixed === 7 * DAY ? 4 * DAY : 0;
    let t = Math.ceil((t0 - offset) / fixed) * fixed + offset;
    for (; t <= t1 && out.length < 400; t += fixed) {
      const r = rankOf(t);
      out.push({ t, rank: fixed < DAY ? r : r === 'time' ? 'day' : r });
    }
    return out;
  }
  const months = MONTH_STEPS.find((m) => m * 28 * DAY >= minStep) ?? Math.ceil(minStep / (365 * DAY)) * 12;
  const start = new Date(t0);
  let y = start.getUTCFullYear();
  let m = Math.ceil(start.getUTCMonth() / months) * months;
  for (let i = 0; i < 400; i++) {
    const t = Date.UTC(y + Math.floor(m / 12), m % 12, 1);
    if (t > t1) break;
    if (t >= t0) out.push({ t, rank: m % 12 === 0 ? 'year' : 'month' });
    m += months;
    if (m >= 12) {
      y += Math.floor(m / 12);
      m %= 12;
    }
  }
  return out;
}
