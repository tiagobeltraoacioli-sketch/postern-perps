/**
 * Simple moving averages of candle closes: a DISPLAY-ONLY reading aid. They are floating-point means of the closes
 * the chart was given (candles are geometry, see types.ts); nothing the user signs, nothing the exchange computes
 * and nothing the page calculates with ever comes from here, and the chart says so where it names them.
 *
 * The chart still invents nothing: an average exists only where `period` candles stand in CONSECUTIVE slots ending
 * at that candle. A missing candle inside the window means no value there (a gap in the line), never a guess.
 */
import type { ChartCandle } from './types';

/** Mean of the `period` closes ending at index `i`, or NaN when the window is short or has a gap. */
export function windowMean(candles: readonly ChartCandle[], slotOf: (i: number) => number, i: number, period: number): number {
  if (!Number.isInteger(period) || period < 2 || i < period - 1 || i >= candles.length) return NaN;
  if (slotOf(i) - slotOf(i - period + 1) !== period - 1) return NaN;
  let sum = 0;
  for (let k = i - period + 1; k <= i; k++) {
    const c = candles[k].c;
    if (!Number.isFinite(c)) return NaN;
    sum += c;
  }
  return sum / period;
}

/** The averages of the candles with index in [i0, i1): values[j] belongs to candle i0 + j (NaN: none). */
export function averageLine(candles: readonly ChartCandle[], slotOf: (i: number) => number, i0: number, i1: number, period: number): Float64Array {
  const out = new Float64Array(Math.max(0, i1 - i0));
  for (let i = i0; i < i1; i++) out[i - i0] = windowMean(candles, slotOf, i, period);
  return out;
}

/**
 * The same mean for a candle list as the app holds it (open times on the interval grid): the readout's value for the
 * candle at index `i`. Null when there is none.
 */
export function averageAt(candles: readonly ChartCandle[], intervalMs: number, i: number, period: number): number | null {
  if (!(intervalMs > 0) || i < 0 || i >= candles.length) return null;
  const t0 = candles[i].t;
  const v = windowMean(candles, (k) => Math.round((candles[k].t - t0) / intervalMs), i, period);
  return Number.isFinite(v) ? v : null;
}

/** Periods accepted from the app: whole numbers from 2 to 400, at most three, without repeats. */
export function cleanPeriods(periods: readonly number[] | undefined): number[] {
  const out: number[] = [];
  for (const p of periods ?? []) if (Number.isInteger(p) && p >= 2 && p <= 400 && !out.includes(p) && out.length < 3) out.push(p);
  return out;
}
