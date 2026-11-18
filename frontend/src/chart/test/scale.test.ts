import { describe, expect, it } from 'vitest';
import {
  MAX_SPACING,
  MIN_SPACING,
  autoRange,
  clampView,
  defaultView,
  priceTicks,
  priceY,
  slotX,
  snapPrice,
  timeTicks,
  xSlot,
  yPrice,
  zoomAt,
} from '../scale';
import { decimalsOf } from '../format';

describe('price ticks', () => {
  it('uses 1, 2, 2.5 or 5 times a power of ten, at least the minimum gap apart', () => {
    for (const [min, max, h] of [
      [66000, 68000, 400],
      [0.1234, 0.1299, 300],
      [1, 100000, 500],
      [99.5, 100.5, 900],
    ] as const) {
      const t = priceTicks({ min, max }, h, 4);
      const m = t.step / 10 ** Math.floor(Math.log10(t.step) + 1e-12);
      expect([1, 2, 2.5, 5].some((x) => Math.abs(x - m) < 1e-9)).toBe(true);
      expect((t.step / (max - min)) * h).toBeGreaterThanOrEqual(44 - 1e-9);
      for (const v of t.values) {
        expect(v).toBeGreaterThanOrEqual(min - 1e-9);
        expect(v).toBeLessThanOrEqual(max + 1e-9);
        expect(Math.abs(v / t.step - Math.round(v / t.step))).toBeLessThan(1e-9);
      }
    }
  });
  it('is never finer than the market decimals nor the tick size', () => {
    const a = priceTicks({ min: 100, max: 100.05 }, 600, 1);
    expect(a.step).toBeGreaterThanOrEqual(0.1);
    expect(a.decimals).toBeLessThanOrEqual(1);
    const b = priceTicks({ min: 100, max: 104 }, 2000, 2, 0.5);
    expect(b.step % 0.5).toBe(0);
    // 2.5 is not offered when it cannot be written at the market decimals.
    const c = priceTicks({ min: 0, max: 30 }, 500, 0);
    expect(decimalsOf(c.step)).toBe(0);
  });
  it('labels are writable at the tick decimals', () => {
    const t = priceTicks({ min: 0.012, max: 0.019 }, 400, 6);
    for (const v of t.values) expect(Number(v.toFixed(t.decimals))).toBeCloseTo(v, 12);
  });
  it('an empty or degenerate range has no ticks', () => {
    expect(priceTicks({ min: 1, max: 1 }, 300, 2).values).toEqual([]);
    expect(priceTicks({ min: 1, max: 2 }, 0, 2).values).toEqual([]);
  });
});

describe('auto range', () => {
  it('covers candles, mark and index and pads', () => {
    const r = autoRange(100, 110, [95, 112, null, undefined, NaN], 0.1)!;
    expect(r.min).toBeCloseTo(95 - 1.7);
    expect(r.max).toBeCloseTo(112 + 1.7);
  });
  it('a flat price still gets a span', () => {
    const r = autoRange(100, 100, [], 0.1)!;
    expect(r.max).toBeGreaterThan(r.min);
  });
  it('nothing to show gives null', () => {
    expect(autoRange(Infinity, -Infinity, [])).toBeNull();
  });
  it('price <-> y round trip', () => {
    const r = { min: 50, max: 150 };
    expect(priceY(150, r, 0, 400)).toBe(0);
    expect(priceY(50, r, 0, 400)).toBe(400);
    expect(yPrice(priceY(123.4, r, 0, 400), r, 0, 400)).toBeCloseTo(123.4, 9);
  });
});

describe('view', () => {
  it('zoom keeps the slot under the pointer in place', () => {
    const v = { spacing: 8, right: 200 };
    for (const x of [0, 137, 500, 800]) {
      const before = xSlot(x, v, 800);
      const z = zoomAt(v, 800, x, 1.7);
      expect(xSlot(x, z, 800)).toBeCloseTo(before, 9);
      expect(slotX(before, z, 800)).toBeCloseTo(x, 9);
    }
  });
  it('spacing stays within limits', () => {
    expect(zoomAt({ spacing: 8, right: 0 }, 800, 400, 1000).spacing).toBe(MAX_SPACING);
    expect(zoomAt({ spacing: 8, right: 0 }, 800, 400, 0.0001).spacing).toBe(MIN_SPACING);
  });
  it('clamp keeps data on screen', () => {
    const far = clampView({ spacing: 10, right: -5000 }, 800, 0, 999);
    expect(far.right).toBeGreaterThanOrEqual(4);
    const future = clampView({ spacing: 10, right: 99999 }, 800, 0, 999);
    expect(xSlot(0, future, 800)).toBeLessThanOrEqual(999 - 4 + 1e-9);
    expect(defaultView(999).right).toBe(1003);
  });
});

describe('time ticks (UTC)', () => {
  const day = 86_400_000;
  it('hour steps are aligned and day boundaries are ranked as days', () => {
    const t0 = Date.UTC(2026, 10, 16, 20, 7);
    const t1 = Date.UTC(2026, 10, 17, 6, 0);
    const ticks = timeTicks(t0, t1, 84 / 3_600_000); // exactly one hour per 84 px
    expect(ticks.length).toBeGreaterThan(5);
    for (const t of ticks) expect(t.t % 3_600_000).toBe(0);
    const midnight = ticks.find((t) => t.t === Date.UTC(2026, 10, 17));
    expect(midnight?.rank).toBe('day');
    expect(ticks.find((t) => t.t === Date.UTC(2026, 10, 16, 21))?.rank).toBe('time');
  });
  it('weekly ticks fall on Mondays, monthly on the first', () => {
    const w = timeTicks(Date.UTC(2026, 0, 1), Date.UTC(2026, 3, 1), 84 / (6 * day));
    for (const t of w) expect(new Date(t.t).getUTCDay()).toBe(1);
    const m = timeTicks(Date.UTC(2025, 5, 3), Date.UTC(2027, 2, 1), 84 / (20 * day));
    for (const t of m) expect(new Date(t.t).getUTCDate()).toBe(1);
    expect(m.find((t) => t.t === Date.UTC(2026, 0, 1))?.rank).toBe('year');
    expect(m.find((t) => t.t === Date.UTC(2026, 6, 1))?.rank).toBe('month');
  });
  it('ticks are at least the gap apart', () => {
    for (const pxPerMs of [1 / 60_000, 1 / 600_000, 1 / 7_200_000, 1 / (3 * day)]) {
      const ticks = timeTicks(0, 400 / pxPerMs, pxPerMs);
      for (let i = 1; i < ticks.length; i++) expect((ticks[i].t - ticks[i - 1].t) * pxPerMs).toBeGreaterThanOrEqual(84 - 1e-6);
    }
  });
});

describe('snapPrice', () => {
  it('rounds to the tick, written exactly', () => {
    expect(snapPrice(67123.47, 0.5, 1)).toBe(67123.5);
    expect(snapPrice(0.123456, undefined, 4)).toBe(0.1235);
    expect(snapPrice(0.1 + 0.2, 0.1, 1)).toBe(0.3);
  });
});
