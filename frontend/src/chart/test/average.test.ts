/** Design pass 3: moving averages (display only), the wording tiers of the level labels, and the volume lane. */
import { describe, expect, it } from 'vitest';
import { averageAt, averageLine, cleanPeriods, windowMean } from '../average';
import { ChartEngine, type EngineData } from '../engine';
import { OWN_AVERAGES, TOKEN_FALLBACK, contrast, readPalette } from '../palette';
import { priceY, rightRoomPx, slotX } from '../scale';
import { VOLUME_SHARE } from '../render';
import type { ChartCandle, ChartLevel } from '../types';
import { FakeCtx, asCtx } from './fake-canvas';
import { walk } from './fixtures';

const M15 = 900_000;
const T0 = Date.UTC(2026, 10, 10);
const flat = (closes: number[], t0 = T0, skip: number[] = []): ChartCandle[] =>
  closes.map((c, i) => ({ t: t0 + i * M15, o: c, h: c + 1, l: c - 1, c, v: 1 })).filter((_, i) => !skip.includes(i));

function setup(w = 900, h = 420) {
  const base = new FakeCtx();
  const overlay = new FakeCtx();
  const frames: ((t: number) => void)[] = [];
  const baseCanvas = document.createElement('canvas');
  const overlayCanvas = document.createElement('canvas');
  const engine = new ChartEngine({ base: baseCanvas, overlay: overlayCanvas, styleRoot: document.documentElement }, {}, { getContext: (c) => asCtx(c === baseCanvas ? base : overlay), requestFrame: (cb) => frames.push(cb), cancelFrame: () => {}, now: () => 0 });
  engine.resize(w, h, 2);
  const run = () => {
    for (const f of frames.splice(0)) f(16);
  };
  return { engine, base, run };
}
const data = (candles: readonly ChartCandle[], extra: Partial<EngineData> = {}): EngineData => ({ symbol: 'BTC-USD', interval: '15m', candles, priceDecimals: 1, sizeDecimals: 4, tickSize: 0.5, mark: null, index: null, levels: [], fills: [], stale: false, candleStyle: 'hollow', wheelZoom: true, pickable: false, ...extra });

describe('moving averages: a mean of closes the chart was given, or nothing', () => {
  const cs = flat([10, 20, 30, 40, 50, 60]);
  const slot = (i: number) => i;
  it('is the plain mean of the last `period` closes', () => {
    expect(windowMean(cs, slot, 2, 3)).toBe(20);
    expect(windowMean(cs, slot, 5, 3)).toBe(50);
    expect(windowMean(cs, slot, 5, 6)).toBe(35);
  });
  it('has no value before `period` candles exist, for a period that is not a whole number of two or more, or out of range', () => {
    expect(windowMean(cs, slot, 1, 3)).toBeNaN();
    expect(windowMean(cs, slot, 5, 1)).toBeNaN();
    expect(windowMean(cs, slot, 5, 2.5)).toBeNaN();
    expect(windowMean(cs, slot, 6, 3)).toBeNaN();
  });
  it('has no value where a candle is missing inside the window (never an interpolation)', () => {
    const gapped = flat([10, 20, 30, 40, 50, 60], T0, [3]);
    const slots = gapped.map((c) => (c.t - T0) / M15);
    // Index 3 is the candle of slot 4: its window of three (slots 2..4) lacks slot 3.
    expect(windowMean(gapped, (i) => slots[i], 3, 3)).toBeNaN();
    expect(windowMean(gapped, (i) => slots[i], 2, 3)).toBe(20);
    expect(averageAt(gapped, M15, 3, 3)).toBeNull();
    expect(averageAt(gapped, M15, 4, 2)).toBe(55);
  });
  it('the readout value (averageAt) is the value the line is drawn from (averageLine), candle for candle', () => {
    const many = walk(120, T0, M15);
    for (const period of [7, 20, 50]) {
      const line = averageLine(many, (i) => i, 0, many.length, period);
      for (let i = 0; i < many.length; i++) {
        const at = averageAt(many, M15, i, period);
        if (Number.isNaN(line[i])) expect(at).toBeNull();
        else expect(at).toBe(line[i]);
      }
      expect(Number.isNaN(line[period - 2])).toBe(true);
      expect(Number.isFinite(line[period - 1])).toBe(true);
    }
  });
  it('accepts at most three whole periods from 2 to 400, without repeats', () => {
    expect(cleanPeriods([20, 50])).toEqual([20, 50]);
    expect(cleanPeriods([20, 20, 1, 0, -5, 2.5, 401, 7, 9, 11])).toEqual([20, 7, 9]);
    expect(cleanPeriods(undefined)).toEqual([]);
  });
  for (const theme of ['dark', 'light'] as const) {
    it(`${theme}: each average ink reaches 3:1 on the background and the surface, and is not a colour with a meaning`, () => {
      const p = readPalette(() => '', theme);
      expect(p.averages).toBe(OWN_AVERAGES[theme]);
      for (const ink of p.averages) {
        expect(contrast(ink, TOKEN_FALLBACK[theme].bg)).toBeGreaterThanOrEqual(3);
        expect(contrast(ink, TOKEN_FALLBACK[theme].surface)).toBeGreaterThanOrEqual(3);
        expect([p.up, p.down, p.net, p.warn, p.danger]).not.toContain(ink);
      }
    });
  }
  it('the engine draws one line per period in its own ink, only over candles that have a value, and none when off', () => {
    const cs = walk(100, T0, M15);
    const off = setup();
    off.engine.setData(data(cs));
    off.run();
    expect(off.engine.getScene()!.averages).toEqual([]);
    const inks = readPalette(() => '', 'dark').averages;
    expect(off.base.ops('stroke').some((c) => inks.includes(c.strokeStyle))).toBe(false);

    const on = setup();
    on.engine.setData(data(cs, { averages: [20, 50] }));
    on.run();
    const s = on.engine.getScene()!;
    expect(s.averages!.map((a) => a.period)).toEqual([20, 50]);
    const strokes = on.base.ops('stroke').filter((c) => inks.includes(c.strokeStyle));
    expect(strokes.map((c) => c.strokeStyle)).toEqual([inks[0], inks[1]]);
    // Every value drawn is the mean of the closes; index 0 of the visible range has none for period 50 when the
    // range starts before candle 49.
    const a50 = s.averages![1].values;
    for (let j = 0; j < a50.length; j++) {
      const i = s.i0 + j;
      if (i < 49) expect(Number.isNaN(a50[j])).toBe(true);
      else expect(a50[j]).toBeCloseTo(cs.slice(i - 49, i + 1).reduce((x, c) => x + c.c, 0) / 50, 9);
    }
  });
});

describe('level labels: the fullest wording that fits beside the newest candle', () => {
  const cs = walk(100, T0, M15);
  const last = cs[99].c;
  const levels: ChartLevel[] = [
    { id: 'pos', kind: 'position', side: 'buy', price: last - 30, priceText: 'E', label: 'Long 0.5000 BTC', short: 'Long 0.5', detail: '+225.60 USDC', detailTone: 'up' },
    { id: 'liq', kind: 'liquidation', side: 'buy', price: last - 60, priceText: 'Q', label: 'Liquidation', short: 'Liq.' },
  ];
  const labelsRightOfCandles = (c: ReturnType<typeof setup>, words: string) => {
    const s = c.engine.getScene()!;
    const newestRight = slotX(s.series.slots[s.series.slots.length - 1], s.view, s.plotW) + s.view.spacing / 2;
    const first = c.base.ops('fillText').filter((t) => words.startsWith(String(t.args[0])) && (t.args[1] as number) < s.plotW && (t.args[1] as number) > 60);
    expect(first.length).toBeGreaterThan(0);
    for (const t of first) expect(t.args[1] as number).toBeGreaterThan(newestRight);
  };
  it('a wide plot writes the label with its detail', () => {
    const c = setup(1200, 420);
    c.engine.setData(data(cs, { levels }));
    c.run();
    const lines = c.base.lines();
    expect(lines.some((l) => l.includes('Long 0.5000 BTC'))).toBe(true);
    expect(lines.some((l) => l.includes('+225.60 USDC'))).toBe(true);
    labelsRightOfCandles(c, 'Long 0.5000 BTC');
  });
  it('a phone-width plot writes the short labels, still right of the newest candle (no label over a candle)', () => {
    const c = setup(366, 420);
    c.engine.setData(data(cs, { levels }));
    c.run();
    const s = c.engine.getScene()!;
    expect(s.plotW).toBeLessThan(520);
    expect(rightRoomPx(s.plotW, true)).toBeGreaterThan(72);
    const lines = c.base.lines();
    expect(lines.some((l) => l.includes('Long 0.5') && !l.includes('0.5000'))).toBe(true);
    expect(lines.some((l) => l.includes('Liq.'))).toBe(true);
    expect(lines.some((l) => l.includes('Liquidation'))).toBe(false);
    expect(lines.some((l) => l.includes('+225.60'))).toBe(false);
    labelsRightOfCandles(c, 'Long 0.5');
  });
});

describe('the volume lane holds only volume', () => {
  it('a level or a live price that falls in the lane has no line drawn there (its tag is pinned at the pane edge)', () => {
    const c = setup();
    const cs = walk(100, T0, M15);
    c.engine.setData(data(cs));
    c.run();
    const s0 = c.engine.getScene()!;
    const laneTop = s0.plotH * (1 - VOLUME_SHARE);
    // A price that maps into the lane with the automatic range.
    const inLane = s0.range!.min + (s0.range!.max - s0.range!.min) * 0.08;
    expect(priceY(inLane, s0.range!, 0, s0.plotH)).toBeGreaterThan(laneTop);
    const levels: ChartLevel[] = [{ id: 'ord', kind: 'order', side: 'buy', price: inLane, priceText: 'O', label: 'Buy 0.2500' }];
    c.base.reset();
    c.engine.setData(data(cs, { levels }));
    c.run();
    const s = c.engine.getScene()!;
    const y = priceY(inLane, s.range!, 0, s.plotH);
    expect(y).toBeGreaterThan(laneTop);
    expect(s.tags.find((t) => t.id === 'ord')!.pinned).toBe('below');
    // No horizontal line across the plot at that height.
    const across = c.base.calls.filter((k, i) => k.op === 'lineTo' && Math.abs((k.args[1] as number) - y) < 1 && (k.args[0] as number) >= s.plotW - 1 && c.base.calls[i - 1]?.op === 'moveTo' && (c.base.calls[i - 1].args[0] as number) === 0);
    expect(across).toHaveLength(0);
  });
});
