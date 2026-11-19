/** Pass 2: the volume lane. With the automatic range no candle enters it and no price is written beside it. */
import { describe, expect, it } from 'vitest';
import { ChartEngine, type EngineData } from '../engine';
import { VOLUME_SHARE } from '../render';
import { priceY } from '../scale';
import { FakeCtx, asCtx } from './fake-canvas';
import { walk } from './fixtures';

const M15 = 900_000;
const T0 = Date.UTC(2026, 10, 10);

function draw(extra: Partial<EngineData> = {}) {
  const base = new FakeCtx();
  const overlay = new FakeCtx();
  const frames: ((t: number) => void)[] = [];
  const baseCanvas = document.createElement('canvas');
  const overlayCanvas = document.createElement('canvas');
  const engine = new ChartEngine({ base: baseCanvas, overlay: overlayCanvas, styleRoot: document.documentElement }, {}, { getContext: (c) => asCtx(c === baseCanvas ? base : overlay), requestFrame: (cb) => frames.push(cb), cancelFrame: () => {}, now: () => 0 });
  engine.resize(900, 420, 1);
  const candles = walk(120, T0, M15);
  engine.setData({ symbol: 'BTC-USD', interval: '15m', candles, priceDecimals: 1, sizeDecimals: 4, tickSize: 0.5, mark: null, index: null, levels: [], fills: [], stale: false, candleStyle: 'hollow', wheelZoom: true, pickable: false, ...extra });
  for (const f of frames.splice(0)) f(16);
  return { engine, base, candles };
}

describe('volume lane', () => {
  it('the lowest low of the visible candles is drawn above the lane, the highest high inside the plot', () => {
    const { engine } = draw();
    const s = engine.getScene()!;
    expect(s.laneClear).toBe(true);
    const laneTop = s.plotH * (1 - VOLUME_SHARE);
    let lo = Infinity;
    let hi = -Infinity;
    for (let i = s.i0; i < s.i1; i++) {
      lo = Math.min(lo, s.series.candles[i].l);
      hi = Math.max(hi, s.series.candles[i].h);
    }
    expect(priceY(lo, s.range!, 0, s.plotH)).toBeLessThan(laneTop);
    expect(priceY(hi, s.range!, 0, s.plotH)).toBeGreaterThan(0);
    // The candles use the space they are given: the lows end within 6% of the plot above the lane.
    expect(laneTop - priceY(lo, s.range!, 0, s.plotH)).toBeLessThan(s.plotH * 0.06);
  });
  it('the lane is named, and no price tick is written beside it', () => {
    const { engine, base } = draw();
    const s = engine.getScene()!;
    const laneTop = s.plotH * (1 - VOLUME_SHARE);
    expect(base.lines()).toContain('Volume');
    // Tick labels are the texts drawn in the axis column (x beyond the plot) above the time axis.
    const axisTexts = base.ops('fillText').filter((c) => (c.args[1] as number) >= s.plotW && (c.args[2] as number) < s.plotH);
    expect(axisTexts.length).toBeGreaterThan(0);
    for (const c of axisTexts) expect(c.args[2] as number).toBeLessThanOrEqual(laneTop);
  });
  it('volume bars stay inside the lane, under its name', () => {
    const { engine, base } = draw();
    const s = engine.getScene()!;
    const laneTop = s.plotH * (1 - VOLUME_SHARE);
    // Volume bars are the fillRects that end exactly at the bottom of the plot.
    const bars = base.ops('fillRect').filter((c) => Math.abs((c.args[1] as number) + (c.args[3] as number) - s.plotH) < 0.01 && (c.args[3] as number) < s.plotH && (c.args[2] as number) < 40);
    expect(bars.length).toBeGreaterThan(50);
    for (const b of bars) expect(b.args[1] as number).toBeGreaterThanOrEqual(laneTop + s.fontPx);
  });
});
