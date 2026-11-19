import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ChartEngine, chartStats, resetChartStats, type EngineData, type EngineHooks } from '../engine';
import type { ChartCandle, ChartLevel } from '../types';
import { FakeCtx, asCtx } from './fake-canvas';
import { walk } from './fixtures';
import { DEFAULT_SPACING, priceY, RIGHT_MARGIN_SLOTS, rightRoomPx, slotX } from '../scale';

const M15 = 900_000;
/** `right` of the default view over 300 candles (newest slot 299) without levels. */
const LATEST = 299 + Math.max(RIGHT_MARGIN_SLOTS, Math.ceil(rightRoomPx(900 - 72, false) / DEFAULT_SPACING));
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
    expect(base.lines().some((l) => l.includes('\u2193 Liquidation'))).toBe(true);
  });
  it('names the live tags and never writes a tick label under a tag', () => {
    const { engine, run, base } = setup();
    const cs = walk(100, T0, M15);
    const last = cs[99].c;
    engine.setData(data(cs, { mark: { price: last + 3, text: 'MARK-TEXT' }, index: { price: last - 40, text: 'IDX' } }));
    run();
    const s = engine.getScene()!;
    const lines = base.lines();
    for (const name of ['Mark', 'Index', 'Last']) expect(lines).toContain(name);
    // Tick labels: text-3 ink, starting at the tick column (plotW + 8).
    const tickYs = base.ops('fillText').filter((c) => c.fillStyle === s.palette.text3 && Math.abs((c.args[1] as number) - (s.plotW + 8)) < 0.01).map((c) => c.args[2] as number);
    expect(tickYs.length).toBeGreaterThan(0);
    for (const y of tickYs) for (const t of s.tags) expect(Math.abs(t.y - y)).toBeGreaterThanOrEqual(s.tagH / 2 + s.fontPx / 2);
  });
  it('crowded levels: a label pushed off its line is tied to the line by a leader ending at the line', () => {
    const { engine, run, base } = setup();
    const cs = walk(100, T0, M15);
    const last = cs[99].c;
    const levels: ChartLevel[] = Array.from({ length: 8 }, (_, i) => ({ id: `o${i}`, kind: 'order', side: 'buy', price: last - 20 - i * 0.5, priceText: `P${i}`, label: `Buy 0.${'5'.repeat(i + 1)}` }));
    engine.setData(data(cs, { levels }));
    run();
    const s = engine.getScene()!;
    const displaced = s.tags.filter((t) => t.id.startsWith('o') && Math.abs(t.y - t.priceY) > 1);
    expect(displaced.length).toBeGreaterThan(0);
    const ends = base.ops('lineTo').map((c) => c.args[1] as number);
    for (const t of displaced) expect(ends.some((y) => Math.abs(y - t.priceY) < 1e-6)).toBe(true);
    // The leaders turn at one common column (they cannot cross): every leader segment ending on a displaced
    // level's line starts at the same x. Pass 3: the labels stand in the breathing room at the RIGHT when they fit
    // (here they do), so the column is looked for on the side where the labels are, not only in the left half; the
    // short stubs from the axis tags (which start at the plot's edge) are not label leaders.
    const starts = new Set<number>();
    base.calls.forEach((c, i) => {
      if (c.op !== 'lineTo') return;
      const [x, y] = c.args as [number, number];
      if (x >= s.plotW - 1 || !displaced.some((t) => Math.abs(t.priceY - y) < 1e-6)) return;
      const m = base.calls[i - 1];
      if (m.op === 'moveTo' && (m.args[0] as number) < s.plotW - 7) starts.add(Math.round((m.args[0] as number) * 1000));
    });
    expect(starts.size).toBe(1);
  });
  it('pass 3: level labels stand right of the newest candle and cover none; on a narrow plot they stand at the left', () => {
    const wide = setup();
    const cs = walk(100, T0, M15);
    const last = cs[99].c;
    const levels: ChartLevel[] = [
      { id: 'pos', kind: 'position', side: 'buy', price: last - 30, priceText: 'E', label: 'Long 0.5000 BTC', detail: '+225.60 USDC', detailTone: 'up' },
      { id: 'ord', kind: 'order', side: 'buy', price: last - 60, priceText: 'O', label: 'Buy 0.2500' },
    ];
    wide.engine.setData(data(cs, { levels }));
    wide.run();
    const s = wide.engine.getScene()!;
    const newestRight = slotX(s.series.slots[s.series.slots.length - 1], s.view, s.plotW) + s.view.spacing / 2;
    expect(s.labelRoom).toBeGreaterThan(120);
    // Every label (its first run of text) starts right of the newest candle.
    const labelTexts = wide.base.ops('fillText').filter((c) => ['Long ', 'Buy '].includes(String(c.args[0])) && (c.args[1] as number) < s.plotW);
    expect(labelTexts.length).toBeGreaterThan(0);
    for (const t of labelTexts) expect(t.args[1] as number).toBeGreaterThan(newestRight);
    expect(wide.base.lines().some((l) => l.includes('Long 0.5000 BTC'))).toBe(true);

    const narrow = setup({}, 340, 420);
    narrow.engine.setData(data(cs, { levels }));
    narrow.run();
    const n = narrow.engine.getScene()!;
    const left = narrow.base.ops('fillText').filter((c) => ['Long ', 'Buy '].includes(String(c.args[0])) && (c.args[1] as number) < n.plotW);
    expect(left.length).toBeGreaterThan(0);
    for (const t of left) expect(t.args[1] as number).toBeLessThan(40);
  });
  it('pass 3: the names of the live prices stand in the breathing room, clear of the newest candle', () => {
    const { engine, run, base } = setup();
    const cs = walk(100, T0, M15);
    engine.setData(data(cs, { mark: { price: cs[99].c, text: 'MARK-TEXT' }, index: { price: cs[99].c - 3, text: 'INDEX-TEXT' } }));
    run();
    const s = engine.getScene()!;
    const newestRight = slotX(s.series.slots[s.series.slots.length - 1], s.view, s.plotW) + s.view.spacing / 2;
    // fillTabular right-aligns a name ending at plotW - 7: its first glyph is the leftmost x of that text.
    const firstGlyph = (word: string) => Math.min(...base.ops('fillText').filter((c) => word.includes(String(c.args[0])) && (c.args[1] as number) > s.plotW - 60 && (c.args[1] as number) < s.plotW).map((c) => c.args[1] as number));
    expect(firstGlyph('Mark')).toBeGreaterThan(newestRight);
    expect(firstGlyph('Index')).toBeGreaterThan(newestRight);
  });
  it('pass 3: tags and pinned labels stop at the top of the volume lane', () => {
    const { engine, run } = setup();
    const cs = walk(100, T0, M15);
    const levels: ChartLevel[] = [{ id: 'liq', kind: 'liquidation', side: 'buy', price: 1000, priceText: 'LIQ', label: 'Liquidation' }];
    engine.setData(data(cs, { levels }));
    run();
    const s = engine.getScene()!;
    const liq = s.tags.find((t) => t.id === 'liq')!;
    expect(liq.pinned).toBe('below');
    expect(liq.y + s.tagH / 2).toBeLessThanOrEqual(s.plotH * 0.8 + 1e-6);
  });
  it('fill markers are drawn under the level labels', () => {
    const { engine, run, base } = setup();
    const cs = walk(100, T0, M15);
    const entry = cs[90].c;
    engine.setData(data(cs, {
      levels: [{ id: 'p', kind: 'position', side: 'buy', price: entry, priceText: 'E', label: 'Long 0.5000 BTC' }],
      fills: [{ id: 'f', time: cs[1].t, price: entry, side: 'buy' }, { id: 'g', time: cs[90].t, price: entry, side: 'sell' }],
    }));
    run();
    const p = engine.getScene()!.palette;
    // A marker is stroked then filled in the up or down colour; a label box is filled then stroked.
    const markers = base.calls.map((c, i) => (c.op === 'fill' && base.calls[i - 1]?.op === 'stroke' && (c.fillStyle === p.up || c.fillStyle === p.down) ? i : -1)).filter((i) => i >= 0);
    const label = base.calls.findIndex((c) => c.op === 'fillText' && String(c.args[0]).startsWith('Long'));
    expect(markers.length).toBeGreaterThan(0);
    expect(label).toBeGreaterThan(Math.max(...markers));
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
    // Pass 3: the default view keeps the breathing room right of the newest candle (scale.ts rightRoomPx: 72 px
    // without levels = 9 slots of 8 px; it was a fixed 4 slots). Slot of the newest candle: 299.
    expect(a.engine.getView().right).toBe(LATEST);
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
    expect(mid).toBeLessThan(LATEST);
    for (let i = 0; i < 30; i++) b.run(16);
    expect(b.engine.getView().right).toBe(LATEST);
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
