import { describe, expect, it } from 'vitest';
import { buildSeries, candleAt, candleProblem, extent, indexRange, slotOfTime } from '../series';
import { walk } from './fixtures';

const M15 = 900_000;
const T0 = Date.UTC(2026, 10, 17);

describe('validation: the chart never repairs data', () => {
  const good = { t: T0, o: 10, h: 12, l: 9, c: 11, v: 1 };
  it('accepts a drawable candle', () => expect(candleProblem(good)).toBeNull());
  it.each([
    ['not-finite', { ...good, c: NaN }],
    ['not-finite', { ...good, v: Infinity }],
    ['high-low', { ...good, h: 10.5 }],
    ['high-low', { ...good, l: 10.5 }],
    ['negative', { ...good, l: -1, o: 0, c: 0, h: 1 }],
    ['negative', { ...good, v: -1 }],
  ] as const)('rejects %s', (reason, c) => {
    expect(candleProblem(c)).toBe(reason);
    const s = buildSeries([good, { ...c, t: T0 + M15 }], M15, null);
    expect(s.candles).toHaveLength(1);
    expect(s.rejected).toBe(1);
    expect(s.rejectReasons[reason]).toBe(1);
  });
  it('rejects a misaligned time and a time out of order', () => {
    const s = buildSeries([good, { ...good, t: T0 + 1000 }, { ...good, t: T0 + 2 * M15 }, { ...good, t: T0 + M15 }], M15, null);
    expect(s.candles.map((c) => c.t)).toEqual([T0, T0 + 2 * M15]);
    expect(s.rejectReasons).toEqual({ misaligned: 1, order: 1 });
  });
  it('a duplicate time is rejected, not merged', () => {
    const s = buildSeries([good, { ...good, c: 11.5, h: 12 }], M15, null);
    expect(s.candles).toHaveLength(1);
    expect(s.candles[0].c).toBe(11);
    expect(s.rejectReasons.order).toBe(1);
  });
});

describe('slots', () => {
  it('a missing candle is an empty slot', () => {
    const cs = walk(10, T0, M15).filter((_, i) => i !== 4);
    const s = buildSeries(cs, M15, null);
    expect(Array.from(s.slots)).toEqual([0, 1, 2, 3, 5, 6, 7, 8, 9]);
    expect(candleAt(s, 4)).toBeNull();
    expect(candleAt(s, 5)?.t).toBe(T0 + 5 * M15);
  });
  it('prepending older history keeps slots of existing candles (fixed anchor)', () => {
    const all = walk(30, T0, M15);
    const first = buildSeries(all.slice(20), M15, null);
    const more = buildSeries(all, M15, first.anchor);
    expect(more.anchor).toBe(first.anchor);
    expect(more.slots[20]).toBe(first.slots[0]);
    expect(more.slots[0]).toBe(-20);
  });
  it('range queries', () => {
    const cs = walk(100, T0, M15);
    const s = buildSeries(cs, M15, null);
    const [i0, i1] = indexRange(s, 10.2, 19.9);
    expect([i0, i1]).toEqual([11, 20]);
    const e = extent(s, i0, i1);
    expect(e.lo).toBe(Math.min(...cs.slice(11, 20).map((c) => c.l)));
    expect(e.hi).toBe(Math.max(...cs.slice(11, 20).map((c) => c.h)));
    expect(e.vMax).toBe(Math.max(...cs.slice(11, 20).map((c) => c.v)));
    expect(extent(s, 5, 5).lo).toBe(Infinity);
    expect(slotOfTime(s, T0 + 3 * M15 + 1)).toBe(3);
    expect(slotOfTime(s, T0 - 1)).toBe(-1);
  });
});
