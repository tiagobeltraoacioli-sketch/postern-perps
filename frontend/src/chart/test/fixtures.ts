/** SYNTHETIC test data (not market data): a deterministic random walk. */
import type { ChartCandle } from '../types';

export function walk(n: number, start: number, intervalMs: number, seed = 7, p0 = 67_000): ChartCandle[] {
  let s = seed >>> 0;
  const rnd = () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
  const out: ChartCandle[] = [];
  let price = p0;
  for (let i = 0; i < n; i++) {
    const o = price;
    const c = Math.round((o * (1 + (rnd() - 0.5) * 0.006)) * 10) / 10;
    const h = Math.round((Math.max(o, c) * (1 + rnd() * 0.002)) * 10) / 10;
    const l = Math.round((Math.min(o, c) * (1 - rnd() * 0.002)) * 10) / 10;
    out.push({ t: start + i * intervalMs, o, h, l, c, v: Math.round(rnd() * 4000) / 100 });
    price = c;
  }
  return out;
}
