import { describe, expect, it } from 'vitest';
import { layoutTags, liquidationZone, undrawableLevels, type TagInput } from '../levels';

const toY = (p: number) => 400 - p; // price 0..400 maps to y 400..0

function check(placed: { y: number; priceY: number }[], h: number, top: number, bottom: number) {
  for (let i = 1; i < placed.length; i++) {
    expect(placed[i].y - placed[i - 1].y).toBeGreaterThanOrEqual(h - 1e-9); // no overlap, order kept
    expect(placed[i].priceY).toBeGreaterThanOrEqual(placed[i - 1].priceY); // a higher price is never written below
  }
  for (const p of placed) {
    expect(p.y - h / 2).toBeGreaterThanOrEqual(top - 1e-9);
    expect(p.y + h / 2).toBeLessThanOrEqual(bottom + 1e-9);
  }
}

describe('axis tags', () => {
  it('sit at their price when free', () => {
    const { placed } = layoutTags([{ id: 'a', kind: 'order', price: 300 }, { id: 'b', kind: 'order', price: 100 }], toY, 0, 400, 20);
    expect(placed.map((p) => p.y)).toEqual([100, 300]);
  });
  it('never overlap and keep price order, also when crowded at an edge', () => {
    let seed = 3;
    for (let round = 0; round < 300; round++) {
      const n = 1 + (round % 15);
      const inputs: TagInput[] = [];
      for (let i = 0; i < n; i++) {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        const price = round % 3 === 0 ? 395 + (seed % 10) : (seed % 600) - 100;
        inputs.push({ id: `t${i}`, kind: (['order', 'position', 'liquidation', 'trigger', 'mark', 'last', 'index'] as const)[seed % 7], price });
      }
      const { placed, dropped } = layoutTags(inputs, toY, 0, 400, 20);
      expect(placed.length + dropped.length).toBe(n);
      check(placed, 20, 0, 400);
    }
  });
  it('a level beyond the visible range is pinned at the edge with its direction', () => {
    const { placed } = layoutTags(
      [
        { id: 'liq', kind: 'liquidation', price: -5000 },
        { id: 'tp', kind: 'trigger', price: 9000 },
        { id: 'pos', kind: 'position', price: 200 },
      ],
      toY,
      0,
      400,
      20,
    );
    const by = Object.fromEntries(placed.map((p) => [p.id, p]));
    expect(by.liq.pinned).toBe('below');
    expect(by.liq.y).toBe(390);
    expect(by.tp.pinned).toBe('above');
    expect(by.tp.y).toBe(10);
    expect(by.pos.pinned).toBeNull();
  });
  it('when they do not fit, the least important go first: liquidation stays', () => {
    const inputs: TagInput[] = [{ id: 'liq', kind: 'liquidation', price: 200 }];
    for (let i = 0; i < 30; i++) inputs.push({ id: `o${i}`, kind: 'order', price: 190 + i });
    inputs.push({ id: 'idx', kind: 'index', price: 201 });
    const { placed, dropped } = layoutTags(inputs, toY, 0, 200, 20);
    expect(placed).toHaveLength(10);
    expect(placed.some((p) => p.id === 'liq')).toBe(true);
    expect(dropped).toContain('idx');
    expect(dropped).toHaveLength(22);
  });
  it('a non-finite price is not placed', () => {
    expect(layoutTags([{ id: 'x', kind: 'order', price: NaN }], toY, 0, 400, 20).placed).toEqual([]);
  });
});

describe('liquidation zone', () => {
  it('lies on the losing side', () => {
    expect(liquidationZone(300, 'buy', 0, 400)).toEqual([300, 400]); // long: price below the line
    expect(liquidationZone(300, 'sell', 0, 400)).toEqual([0, 300]); // short: price above
    expect(liquidationZone(-50, 'buy', 0, 400)).toEqual([0, 400]); // long liquidation above the screen: all of it
    expect(liquidationZone(500, 'buy', 0, 400)).toBeNull();
  });
});

describe('undrawableLevels (R395-32)', () => {
  it('returns exactly the levels whose price is not a finite number, in input order', () => {
    const out = undrawableLevels([
      { id: 'a', label: 'A', kind: 'liquidation', price: Number.NaN },
      { id: 'b', label: 'B', kind: 'order', price: 1 },
      { id: 'c', label: 'C', kind: 'trigger', price: Number.NEGATIVE_INFINITY },
      { id: 'd', label: 'D', kind: 'position', price: 0 },
    ]);
    expect(out.map((l) => l.id)).toEqual(['a', 'c']);
  });
  it('every level is either placed-or-dropped by layoutTags or reported undrawable: none vanishes', () => {
    const inputs = [1, Number.NaN, 5, Number.POSITIVE_INFINITY, 3].map((price, i) => ({ id: `l${i}`, label: `L${i}`, kind: 'order' as const, price }));
    const layout = layoutTags(inputs, (p) => 100 - p * 10, 0, 100, 10);
    const seen = new Set([...layout.placed.map((t) => t.id), ...layout.dropped, ...undrawableLevels(inputs).map((l) => l.id)]);
    expect(seen.size).toBe(inputs.length);
  });
});

