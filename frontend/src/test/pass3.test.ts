/** Design pass 3: the pure pieces behind the depth view, the book's bars, the chart's short labels, the split bundle. */
import { describe, expect, it } from 'vitest';
import { depthModel, depthPath } from '../trade/DepthChart';
import { BAR_SHARE } from '../trade/BookAndTrades';
import { trimZeros } from '../trade/ChartPanel';
import { notional, type Level } from '../lib/calc';
import appSource from '../App.tsx?raw';

const E8 = 100_000_000n;
const px = (v: number) => BigInt(Math.round(v * 10)) * (E8 / 10n);
const sz = (v: number) => BigInt(Math.round(v * 10_000)) * (E8 / 10_000n);
const L = (p: number, s: number): Level => ({ px: px(p), size: sz(s) });

describe('depth view: cumulative size of the levels the exchange sent', () => {
  const bids = [L(99.5, 1), L(99, 2), L(98, 3), L(90, 50)];
  const asks = [L(100.5, 2), L(101, 2), L(102, 1)];
  const m = depthModel(bids, asks);
  it('the mid is the middle of the best bid and ask; the window reaches equally far on both sides', () => {
    expect(m.mid).toBe(px(100));
    // Bids reach 10 below the mid, asks 2 above: the window is 2 on each side (the shallower side).
    expect(m.lo).toBe(px(98));
    expect(m.hi).toBe(px(102));
  });
  it('each side accumulates from the best price outward, and stops at the window', () => {
    expect(m.bids.map((p) => p.cum)).toEqual([sz(1), sz(3), sz(6)]);
    expect(m.asks.map((p) => p.cum)).toEqual([sz(2), sz(4), sz(5)]);
    expect(m.bids.some((p) => p.px === px(90))).toBe(false);
    expect(m.max).toBe(sz(6));
  });
  it('the USD figure is the sum of each level at its own price (the same integer notional the book shows)', () => {
    expect(m.bids[2].usd).toBe(notional(sz(1), px(99.5)) + notional(sz(2), px(99)) + notional(sz(3), px(98)));
  });
  it('one side alone is drawn over its own prices; an empty book has no picture', () => {
    const only = depthModel(bids, []);
    expect(only.mid).toBeNull();
    expect(only.lo).toBe(px(90));
    expect(only.hi).toBe(px(99.5));
    expect(only.bids).toHaveLength(4);
    expect(only.asks).toHaveLength(0);
    const none = depthModel([], []);
    expect(none.max).toBe(0n);
    expect(depthPath(none.bids, none, 'left')).toBe('');
  });
  it('the outlines are closed steps inside the box: bids fall to the left edge, asks rise to the right edge', () => {
    const b = depthPath(m.bids, m, 'left');
    const a = depthPath(m.asks, m, 'right');
    for (const d of [b, a]) {
      expect(d.startsWith('M')).toBe(true);
      expect(d.endsWith('Z')).toBe(true);
      // Only horizontal and vertical moves: a step, never a slope between two levels.
      expect(/^M[\d.]+ [\d.]+( [VH][\d.]+)+ Z$/.test(d)).toBe(true);
      for (const n of d.match(/[\d.]+/g)!.map(Number)) {
        expect(n).toBeGreaterThanOrEqual(0);
        expect(n).toBeLessThanOrEqual(1000);
      }
    }
    expect(b).toContain(' H0 ');
    expect(a).toContain(' H1000 ');
    // The best bid stands left of the mid, the best ask right of it.
    expect(Number(/^M([\d.]+)/.exec(b)![1])).toBeLessThan(500);
    expect(Number(/^M([\d.]+)/.exec(a)![1])).toBeGreaterThan(500);
  });
});

describe('order book: a depth bar never reaches under a price', () => {
  it('a full bar spans less than the size and total columns', () => {
    expect(BAR_SHARE).toBeGreaterThan(0.4);
    expect(BAR_SHARE).toBeLessThan(2 / 3);
  });
});

describe('chart: short labels for a narrow plot', () => {
  it('a size loses its trailing zeros and nothing else', () => {
    expect(trimZeros('0.5000')).toBe('0.5');
    expect(trimZeros('4.200')).toBe('4.2');
    expect(trimZeros('12.0000')).toBe('12');
    expect(trimZeros('100')).toBe('100');
    expect(trimZeros('0.0001')).toBe('0.0001');
    expect(trimZeros('1,200.50')).toBe('1,200.5');
  });
});

describe('route-level code splitting', () => {
  const app = appSource;
  it('the shell imports no page but the home page and the not-found page statically', () => {
    const statics = [...app.matchAll(/^import [^;]*? from '(\.\/(?:pages|trade)\/[^']+)';/gm)].map((m) => m[1]);
    expect(statics.sort()).toEqual(['./pages/Home', './pages/NotFound']);
  });
  it('every other page is loaded when its address is opened', () => {
    for (const page of ['./trade/TradePage', './pages/Markets', './pages/Portfolio', './pages/Funds', './pages/History', './pages/Funding', './pages/Agents', './pages/System', './pages/Legal']) {
      expect(app).toContain(`import('${page}')`);
    }
  });
});
