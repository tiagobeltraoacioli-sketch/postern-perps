import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ChartEngine, chartStats, resetChartStats, type EngineData, type EngineHooks } from '../engine';
import type { ChartCandle, ChartLevel } from '../types';
import { FakeCtx, asCtx } from './fake-canvas';
import { walk } from './fixtures';
import { priceY, slotX } from '../scale';

const M15 = 900_000;
const T0 = Date.UTC(2026, 10, 10);

function setup(hooks: EngineHooks = {}, w = 900, h = 420) {
  const base = new FakeCtx();
  const overlay = new FakeCtx();
  const frames: ((t: number) => void)[] = [];
  let now = 0;
  const baseCanvas = document.createElement('canvas');
  const overlayCanvas = document.createElement('canvas');
  const engine = new ChartEngine(
    { base: baseCanvas, overlay: overlayCanvas, styleRoot: document.documentElement },
    hooks,
    {
      getContext: (c) => asCtx(c === baseCanvas ? base : overlay),
      requestFrame: (cb) => frames.push(cb),
      cancelFrame: () => {},
      now: () => now,
    },
  );
  engine.resize(w, h, 2);
  const run = (advance = 16) => {
    now += advance;
    const pending = frames.splice(0);
    for (const f of pending) f(now);
    return pending.length;
  };
  return { engine, base, overlay, run, frames, advance: (ms: number) => (now += ms) };
}

function data(candles: readonly ChartCandle[], extra: Partial<EngineData> = {}): EngineData {
  return {
    symbol: 'BTC-USD',
    interval: '15m',
    candles,
    priceDecimals: 1,
    sizeDecimals: 4,
    tickSize: 0.5,
    mark: null,
    index: null,
    levels: [],
    fills: [],
    stale: false,
    candleStyle: 'hollow',
    wheelZoom: true,
    pickable: false,
    ...extra,
  };
}

beforeEach(() => resetChartStats());

describe('frames', () => {
  it('coalesces any number of updates into one base frame', () => {
    const { engine, run } = setup();
    const cs = walk(300, T0, M15);
    for (let i = 0; i < 250; i++) {
      const last = cs[cs.length - 1];
      const c = Math.round((last.c + (i % 7) - 3) * 10) / 10;
      engine.setData(data([...cs.slice(0, -1), { ...last, c, h: Math.max(last.h, c), l: Math.min(last.l, c) }]));
    }
    expect(run()).toBe(1);
    expect(chartStats.baseFrames).toBe(1);
    expect(chartStats.updates).toBe(250);
    expect(chartStats.frameMs).toHaveLength(1);
  });
  it('a pointer move redraws only the overlay', () => {
    const { engine, run, base } = setup();
    engine.setData(data(walk(100, T0, M15)));
    run();
    base.reset();
    engine.pointerMove(1, 300, 200);
    run();
    expect(base.calls).toHaveLength(0);
    expect(chartStats.overlayFrames).toBe(2);
  });
});

describe('it draws the data given and nothing else', () => {
  it('one candle per visible valid candle; an empty slot draws nothing', () => {
    const { engine, run } = setup();
    const cs = walk(40, T0, M15).filter((_, i) => i !== 30 && i !== 31);
    engine.setData(data(cs));
    run();
    const scene = engine.getScene()!;
    expect(engine.lastDrawn.candles).toBe(scene.i1 - scene.i0);
    expect(engine.lastDrawn.candles).toBe(38);
  });
  it('invalid candles are refused and counted, not drawn', () => {
    const { engine, run } = setup();
    const cs = walk(20, T0, M15);
    const bad = [...cs.slice(0, 10), { ...cs[10], h: cs[10].l - 1 }, ...cs.slice(11)];
    engine.setData(data(bad));
    run();
    expect(engine.getSeries().rejected).toBe(1);
    expect(engine.lastDrawn.candles).toBe(19);
  });
  it('writes the app texts of mark, index and levels verbatim on the axis', () => {
    const { engine, run, base } = setup();
    const cs = walk(100, T0, M15);
    const last = cs[cs.length - 1].c;
    const levels: ChartLevel[] = [
      { id: 'position:BTC-USD', kind: 'position', side: 'buy', price: last - 50, priceText: '66,950.0', label: 'Long 0.5000 BTC', detail: '+225.60 USDC', detailTone: 'up' },
      { id: 'o1', kind: 'order', side: 'sell', price: last + 40, priceText: 'X-ORDER-TEXT', label: 'Sell 0.2500' },
    ];
    engine.setData(data(cs, { mark: { price: last + 3, text: 'MARK-TEXT' }, index: { price: last - 4, text: 'INDEX-TEXT' }, levels }));
    run();
    const lines = base.lines();
    for (const t of ['66,950.0', 'X-ORDER-TEXT', 'MARK-TEXT', 'INDEX-TEXT', 'Long 0.5000 BTC', '+225.60 USDC', 'Sell 0.2500']) {
      expect(lines.some((l) => l.includes(t)), t).toBe(true);
    }
  });
  it('a liquidation far outside the range is pinned at the edge (never out of sight) and does not squash the candles', () => {
    const { engine, run, base } = setup();
    const cs = walk(100, T0, M15);
    engine.setData(data(cs));
    run();
    const before = engine.getScene()!.range!;
    base.reset();
    engine.setData(data(cs, { levels: [{ id: 'liq', kind: 'liquidation', side: 'buy', price: 1000, priceText: '1,000.0', label: 'Liquidation' }] }));
    run();
    const scene = engine.getScene()!;
    expect(scene.range).toEqual(before);
    const tag = scene.tags.find((t) => t.id === 'liq')!;
    expect(tag.pinned).toBe('below');
    expect(tag.y + scene.tagH / 2).toBeLessThanOrEqual(scene.plotH + 1e-9);
    expect(base.lines().some((l) => l.includes('1,000.0'))).toBe(true);
    expect(base.lines().some((l) => l.includes('Liquidation'))).toBe(true);
  });
  it('a level with a non-finite price is not drawn', () => {
    const { engine, run } = setup();
    engine.setData(data(walk(50, T0, M15), { levels: [{ id: 'x', kind: 'order', side: 'buy', price: NaN, priceText: '?', label: 'Buy' }] }));
    run();
    expect(engine.getScene()!.tags.some((t) => t.id === 'x')).toBe(false);
    expect(engine.lastDrawn.levels).toBe(0);
  });
  it('a new price is drawn at its place the frame it arrives, also with motion on (no tween)', () => {
    const { engine, run } = setup();
    engine.reducedMotion = false;
    const cs = walk(60, T0, M15);
    engine.setData(data(cs, { mark: { price: cs[59].c, text: 'a' } }));
    run();
    const next = cs[59].c + 30;
    engine.setData(data(cs, { mark: { price: next, text: 'b' } }));
    run();
    const s = engine.getScene()!;
    const markTag = s.tags.find((t) => t.kind === 'mark')!;
    expect(markTag.priceY).toBeCloseTo(priceY(next, s.range!, 0, s.plotH), 9);
  });
  it('stale: the live candle and mark are drawn muted', () => {
    const { engine, run, base } = setup();
    const cs = walk(60, T0, M15);
    engine.setData(data(cs, { stale: true, mark: { price: cs[59].c, text: 'm' } }));
    run();
    const muted = engine.getScene()!.palette.text3;
    const bodies = base.calls.filter((c) => (c.op === 'fillRect' || c.op === 'strokeRect') && (c.fillStyle === muted || c.strokeStyle === muted));
    expect(bodies.length).toBeGreaterThan(0);
  });
});

describe('view', () => {
  it('follows the newest candle as it appends; stays put when the user panned away', () => {
    const { engine, run } = setup();
    const cs = walk(200, T0, M15);
    engine.setData(data(cs.slice(0, 150)));
    run();
    const r0 = engine.getView().right;
    engine.setData(data(cs.slice(0, 151)));
    run();
    expect(engine.getView().right).toBe(r0 + 1);
    engine.pan(-40);
    run();
    expect(engine.isFollowing()).toBe(false);
    const r1 = engine.getView().right;
    engine.setData(data(cs.slice(0, 152)));
    run();
    expect(engine.getView().right).toBe(r1);
  });
  it('prepending history keeps the view anchored and asks once per oldest candle', () => {
    const onReachStart = vi.fn();
    const { engine, run } = setup({ onReachStart });
    const cs = walk(400, T0, M15);
    engine.setData(data(cs.slice(300)));
    run();
    engine.goToOldest();
    run();
    expect(onReachStart).toHaveBeenCalledTimes(1);
    expect(onReachStart).toHaveBeenLastCalledWith(cs[300].t);
    engine.pan(1);
    run();
    engine.pan(-1);
    run();
    expect(onReachStart).toHaveBeenCalledTimes(1);
    const right = engine.getView().right;
    engine.setData(data(cs.slice(200)));
    run();
    expect(engine.getView().right).toBe(right);
    engine.goToOldest();
    run();
    expect(onReachStart).toHaveBeenCalledTimes(2);
    expect(onReachStart).toHaveBeenLastCalledWith(cs[200].t);
  });
  it('wheel zoom keeps the candle under the pointer; horizontal wheel pans', () => {
    const { engine, run } = setup();
    engine.setData(data(walk(500, T0, M15)));
    run();
    const s = engine.getScene()!;
    const slot = 470;
    const x = slotX(slot, engine.getView(), s.plotW);
    expect(engine.wheel(x, 0, -120, false)).toBe(true);
    run();
    expect(slotX(slot, engine.getView(), engine.getScene()!.plotW)).toBeCloseTo(x, 6);
    const r = engine.getView().right;
    engine.wheel(x, -80, 2, false);
    expect(engine.getView().right).toBeLessThan(r);
  });
  it('wheelZoom false lets the page scroll (event unused) but ctrl+wheel still zooms', () => {
    const { engine, run } = setup();
    engine.setData(data(walk(100, T0, M15), { wheelZoom: false }));
    run();
    expect(engine.wheel(100, 0, 50, false)).toBe(false);
    expect(engine.wheel(100, 0, 50, true)).toBe(true);
  });
  it('symbol change resets the view', () => {
    const { engine, run } = setup();
    engine.setData(data(walk(300, T0, M15)));
    run();
    engine.pan(-100);
    run();
    engine.setData({ ...data(walk(300, T0, M15, 9, 3000)), symbol: 'ETH-USD' });
    run();
    expect(engine.isFollowing()).toBe(true);
  });
  it('go to latest: animated over frames with motion, immediate with reduced motion', () => {
    const a = setup();
    a.engine.setData(data(walk(300, T0, M15)));
    a.run();
    a.engine.pan(-100);
    a.run();
    a.engine.reducedMotion = true;
    a.engine.goToLatest();
    a.run();
    expect(a.engine.getView().right).toBe(303);
    const b = setup();
    b.engine.reducedMotion = false;
    b.engine.setData(data(walk(300, T0, M15)));
    b.run();
    b.engine.pan(-100);
    b.run();
    b.engine.goToLatest();
    b.run(100);
    const mid = b.engine.getView().right;
    expect(mid).toBeGreaterThan(203);
    expect(mid).toBeLessThan(303);
    for (let i = 0; i < 30; i++) b.run(16);
    expect(b.engine.getView().right).toBe(303);
  });
});

describe('price pick', () => {
  it('keyboard: arrows choose a price on the tick grid, Enter picks it', () => {
    const onPricePick = vi.fn();
    const { engine, run } = setup({ onPricePick });
    engine.setData(data(walk(100, T0, M15), { pickable: true }));
    run();
    expect(engine.key('Enter', false)).toBe(false); // nothing chosen yet
    engine.key('ArrowUp', false);
    run();
    engine.key('Enter', false);
    expect(onPricePick).toHaveBeenCalledTimes(1);
    const p = onPricePick.mock.calls[0][0] as number;
    expect((p * 2) % 1).toBe(0); // tick 0.5
  });
  it('pointer: the + box next to the crosshair tag picks the snapped price', () => {
    const onPricePick = vi.fn();
    const { engine, run } = setup({ onPricePick });
    engine.setData(data(walk(100, T0, M15), { pickable: true }));
    run();
    const s = engine.getScene()!;
    engine.pointerMove(1, s.plotW - 10, 150);
    run();
    engine.pointerDown(1, s.plotW - 10, 150);
    expect(onPricePick).toHaveBeenCalledTimes(1);
    const p = onPricePick.mock.calls[0][0] as number;
    expect((p * 2) % 1).toBe(0);
    expect(Math.abs(priceY(p, s.range!, 0, s.plotH) - 150)).toBeLessThan(5);
  });
});
