/** Design pass 3: the pure pieces behind the depth view, the book's bars, the chart's short labels, the split bundle. */
import { describe, expect, it } from 'vitest';
import { depthModel, depthPath } from '../trade/DepthChart';
import { BAR_SHARE } from '../trade/BookAndTrades';
import { trimZeros } from '../trade/ChartPanel';
import { notional, type Level } from '../lib/calc';
import appSource from '../App.tsx?raw';
import { openNotionalOf } from '../lib/convert';
import { allBandsWords, bandFor, currentBand, exitBand, notePublishedMarkets } from '../lib/exit-band';
import { MAX_SLIPPAGE_PPM, maxSlippagePpm } from '../trade/order-builder';

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

describe('F434-4: the exchange\'s exact open notional', () => {
  // The running node's own figures (stack fin4, 2026-11-19): size -0.4 at entry 62156.
  const size = -sz(0.4);
  const entry = 62156n * E8;
  it('is used as given when it is the position\'s own figure', () => {
    expect(openNotionalOf('-248624000000000000000', size, entry)).toBe(-248624000000000000000n);
    // An entry price rounded on the wire: the exact notional differs a little from size x entry and is still taken.
    expect(openNotionalOf('-248624000000000012345', size, entry)).toBe(-248624000000000012345n);
  });
  it('is refused (the page falls back to size x entry) when absent, malformed, of the wrong sign or far off', () => {
    expect(openNotionalOf(undefined, size, entry)).toBeNull();
    expect(openNotionalOf(248624, size, entry)).toBeNull();
    expect(openNotionalOf('-2.48e20', size, entry)).toBeNull();
    expect(openNotionalOf('0x10', size, entry)).toBeNull();
    expect(openNotionalOf('248624000000000000000', size, entry)).toBeNull();
    expect(openNotionalOf('-300000000000000000000', size, entry)).toBeNull();
    expect(openNotionalOf('0', size, entry)).toBeNull();
  });
});

describe('F434-5 / F434-6: the exchange\'s own published bands come first, the launch table is the stated fallback', () => {
  const table = exitBand('BTC-USD')!;
  it('without published figures the launch table is used and named', () => {
    notePublishedMarkets([{ symbol: 'BTC-USD', oracle: { sources: [], minSources: 1 } }]);
    expect(bandFor('BTC-USD')).toEqual({ band: table, source: 'launch table', differs: false });
    expect(allBandsWords().startsWith('By the launch table: ')).toBe(true);
  });
  it('published figures are used as given, and a difference from the launch table is flagged', () => {
    const oracle = { exitHaircutBps: table.baseBps + 100, exitHaircutPerMinBps: table.perMinuteBps, exitMaxHaircutBps: table.maxBps, maxHaltMs: table.maxHaltMs };
    notePublishedMarkets([{ symbol: 'BTC-USD', oracle }]);
    const found = bandFor('BTC-USD')!;
    expect(found.source).toBe('exchange');
    expect(found.band.baseBps).toBe(table.baseBps + 100);
    expect(found.differs).toBe(true);
    expect(allBandsWords().startsWith('As the exchange publishes: BTC-USD ')).toBe(true);
    notePublishedMarkets([{ symbol: 'BTC-USD', oracle: { exitHaircutBps: table.baseBps, exitHaircutPerMinBps: table.perMinuteBps, exitMaxHaircutBps: table.maxBps, maxHaltMs: table.maxHaltMs } }]);
    expect(bandFor('BTC-USD')).toEqual({ band: table, source: 'exchange', differs: false });
  });
  it('malformed published figures are not used (the fallback stays)', () => {
    notePublishedMarkets([{ symbol: 'BTC-USD', oracle: { exitHaircutBps: '500', exitHaircutPerMinBps: 100, exitMaxHaircutBps: 5000, maxHaltMs: 3_600_000 } }, { symbol: 'ETH-USD', oracle: { exitHaircutBps: 500, exitHaircutPerMinBps: 100, exitMaxHaircutBps: 10_000, maxHaltMs: 1 } }]);
    expect(bandFor('BTC-USD')!.source).toBe('launch table');
    expect(bandFor('ETH-USD')!.source).toBe('launch table');
    notePublishedMarkets([]);
  });
  it('the engine\'s current band is shown only when it has the documented shape', () => {
    const ok = { exitBand: { low: '63887.31', high: '70612.29', settlePrice: '67249.8', settleDue: false, asOf: 1794878600000 } };
    expect(currentBand(ok)).toEqual(ok.exitBand);
    expect(currentBand({ exitBand: null })).toBeNull();
    expect(currentBand({})).toBeNull();
    expect(currentBand({ exitBand: { ...ok.exitBand, low: '7e4' } })).toBeNull();
    expect(currentBand({ exitBand: { ...ok.exitBand, low: '80000', high: '70000' } })).toBeNull();
    expect(currentBand({ exitBand: { ...ok.exitBand, settleDue: 'no' } })).toBeNull();
  });
  it('the widest price bound of a market order is the published band, else 10%', () => {
    expect(maxSlippagePpm({})).toBe(MAX_SLIPPAGE_PPM);
    expect(maxSlippagePpm({ marketSlippagePpm: null })).toBe(MAX_SLIPPAGE_PPM);
    expect(maxSlippagePpm({ marketSlippagePpm: 50_000n })).toBe(50_000n);
    expect(maxSlippagePpm({ marketSlippagePpm: 500_000n })).toBe(MAX_SLIPPAGE_PPM);
  });
});
