/**
 * The candle series as the chart holds it: validated, placed in slots (see scale.ts), with range queries.
 *
 * The chart never repairs data. A candle that cannot be drawn truthfully is REJECTED (not drawn, counted, and the
 * reason kept for the measurement page): a non-finite field, a high below the open/close or a low above them, a
 * negative low or volume, an open time not aligned to the interval, or a time not after the previous candle's.
 * An open time absent from the data is an empty slot.
 */
import type { ChartCandle } from './types';

export type RejectReason = 'not-finite' | 'high-low' | 'negative' | 'misaligned' | 'order';

export type SeriesData = {
  /** Slot origin: open time of slot 0 (fixed per view; see Series.anchor). */
  readonly anchor: number;
  readonly intervalMs: number;
  /** Accepted candles, oldest first, strictly increasing slot. */
  readonly candles: readonly ChartCandle[];
  /** slots[i] is the slot of candles[i]. */
  readonly slots: Int32Array;
  readonly rejected: number;
  readonly rejectReasons: Readonly<Partial<Record<RejectReason, number>>>;
};

/** Why a candle is refused, or null when it can be drawn as given. */
export function candleProblem(c: ChartCandle): RejectReason | null {
  const { t, o, h, l, c: close, v } = c;
  if (![t, o, h, l, close, v].every(Number.isFinite)) return 'not-finite';
  if (h < o || h < close || l > o || l > close || h < l) return 'high-low';
  if (l < 0 || v < 0) return 'negative';
  return null;
}

/**
 * Validate and slot `input` against `anchor` (a time on the interval grid; null: the first acceptable candle's).
 */
export function buildSeries(input: readonly ChartCandle[], intervalMs: number, anchor: number | null): SeriesData {
  const candles: ChartCandle[] = [];
  const slotList: number[] = [];
  const reasons: Partial<Record<RejectReason, number>> = {};
  let rejected = 0;
  const reject = (r: RejectReason) => {
    rejected++;
    reasons[r] = (reasons[r] ?? 0) + 1;
  };
  let origin = anchor;
  let lastT = -Infinity;
  for (const c of input) {
    const problem = candleProblem(c);
    if (problem) {
      reject(problem);
      continue;
    }
    if (origin === null) origin = c.t;
    const q = (c.t - origin) / intervalMs;
    const slot = Math.round(q);
    if (Math.abs(q - slot) > 1e-9 || !Number.isSafeInteger(slot) || Math.abs(slot) > 2 ** 31 - 1) {
      reject('misaligned');
      continue;
    }
    if (!(c.t > lastT)) {
      reject('order');
      continue;
    }
    lastT = c.t;
    candles.push(c);
    slotList.push(slot);
  }
  return {
    anchor: origin ?? 0,
    intervalMs,
    candles,
    slots: Int32Array.from(slotList),
    rejected,
    rejectReasons: reasons,
  };
}

/** Index of the first candle whose slot is >= `slot` (candles.length if none). */
export function lowerBound(slots: Int32Array, slot: number): number {
  let lo = 0;
  let hi = slots.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (slots[mid] < slot) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/** Candle index range [i0, i1) of candles with slot in [s0, s1]. */
export function indexRange(data: SeriesData, s0: number, s1: number): [number, number] {
  return [lowerBound(data.slots, Math.ceil(s0)), lowerBound(data.slots, Math.floor(s1) + 1)];
}

export type Extent = { lo: number; hi: number; vMax: number };

/** Low, high and largest volume of candles [i0, i1); lo = +Infinity when empty. */
export function extent(data: SeriesData, i0: number, i1: number): Extent {
  let lo = Infinity;
  let hi = -Infinity;
  let vMax = 0;
  const cs = data.candles;
  for (let i = i0; i < i1; i++) {
    const c = cs[i];
    if (c.l < lo) lo = c.l;
    if (c.h > hi) hi = c.h;
    if (c.v > vMax) vMax = c.v;
  }
  return { lo, hi, vMax };
}

/** The candle at `slot`, or null for an empty slot. */
export function candleAt(data: SeriesData, slot: number): ChartCandle | null {
  const i = lowerBound(data.slots, slot);
  return i < data.slots.length && data.slots[i] === slot ? data.candles[i] : null;
}

/** The slot of time `t` (the candle that contains it). */
export function slotOfTime(data: Pick<SeriesData, 'anchor' | 'intervalMs'>, t: number): number {
  return Math.floor((t - data.anchor) / data.intervalMs);
}

export function slotTime(data: Pick<SeriesData, 'anchor' | 'intervalMs'>, slot: number): number {
  return data.anchor + slot * data.intervalMs;
}
