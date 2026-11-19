/** Design pass 2: the pure pieces behind what is drawn (order book rows and flashes, sparklines). */
import { describe, expect, it } from 'vitest';
import { changedLevels, rowsThatFit } from '../trade/BookAndTrades';
import { sparkPath } from '../pages/Markets';

describe('order book: whole rows only', () => {
  it('the rows per side are what fits beside the spread row, rounded DOWN (never a cut row)', () => {
    expect(rowsThatFit(40 + 2 * 22 * 7, 22, 40)).toBe(7);
    expect(rowsThatFit(40 + 2 * 22 * 7 + 43, 22, 40)).toBe(7);
    expect(rowsThatFit(40 + 2 * 22 * 7 + 44, 22, 40)).toBe(8);
    expect(rowsThatFit(40 + 2 * 22 * 7 - 1, 22, 40)).toBe(6);
  });
  it('at least one row, at most forty; an unmeasured panel keeps the default', () => {
    expect(rowsThatFit(10, 22, 40)).toBe(1);
    expect(rowsThatFit(100_000, 22, 40)).toBe(40);
    expect(rowsThatFit(0, 22, 40)).toBe(11);
    expect(rowsThatFit(Number.NaN, 22, 40)).toBe(11);
    expect(rowsThatFit(500, 0, 40)).toBe(11);
  });
  it('for every height the rows drawn fit inside it', () => {
    for (let h = 60; h < 1400; h += 7) {
      const rows = rowsThatFit(h, 22, 40);
      if (rows > 1 && rows < 40) expect(2 * rows * 22 + 40).toBeLessThanOrEqual(h);
    }
  });
});

describe('order book: which rows flash', () => {
  const before = new Map([
    ['100', 5n],
    ['101', 7n],
  ]);
  it('a level that grew, shrank or appeared flashes in that direction; an unchanged one does not', () => {
    const changed = changedLevels(before, [
      { px: 100n, size: 5n },
      { px: 101n, size: 9n },
      { px: 102n, size: 1n },
    ]);
    expect([...changed]).toEqual([
      ['101', 'up'],
      ['102', 'up'],
    ]);
    expect(changedLevels(before, [{ px: 101n, size: 2n }]).get('101')).toBe('down');
    expect(changedLevels(before, [...before].map(([px, size]) => ({ px: BigInt(px), size }))).size).toBe(0);
  });
});

describe('sparkline geometry', () => {
  it('spans the box, first point at the left edge, last at the right, highest close at the top', () => {
    const d = sparkPath([10, 20, 15], 120, 32)!;
    expect(d.startsWith('M0.00 31.00')).toBe(true);
    expect(d).toContain('L60.00 1.00');
    expect(d.endsWith('L120.00 16.00')).toBe(true);
  });
  it('a flat series is a line through the middle; fewer than two finite points draw nothing', () => {
    expect(sparkPath([5, 5, 5], 120, 32)).toBe('M0.00 16.00L60.00 16.00L120.00 16.00');
    expect(sparkPath([5], 120, 32)).toBeNull();
    expect(sparkPath([], 120, 32)).toBeNull();
    expect(sparkPath([Number.NaN, 5], 120, 32)).toBeNull();
  });
});
