import { describe, expect, it } from 'vitest';
import { fillTabular, resetTextCache, tabularWidth } from '../text';

function ctx() {
  const calls: [string, number][] = [];
  return {
    calls,
    font: '12px x',
    textAlign: 'left' as CanvasTextAlign,
    textBaseline: 'middle' as CanvasTextBaseline,
    // Proportional digits: '1' narrow, '0' wide.
    measureText: (t: string) => ({ width: [...t].reduce((w, ch) => w + (ch === '1' ? 4 : ch >= '0' && ch <= '9' ? 7 : 3), 0) }) as TextMetrics,
    fillText: (t: string, x: number) => calls.push([t, x]),
  };
}

describe('tabular figures on canvas', () => {
  it('every digit takes the widest digit advance', () => {
    resetTextCache();
    const c = ctx();
    expect(tabularWidth(c, '1111')).toBe(28);
    expect(tabularWidth(c, '1,000.0')).toBe(5 * 7 + 2 * 3);
  });
  it('the same digit position lands at the same x whatever the digits', () => {
    resetTextCache();
    const a = ctx();
    const b = ctx();
    fillTabular(a, '11,111.1', 100, 0, 'right');
    fillTabular(b, '90,000.0', 100, 0, 'right');
    const xa = a.calls.filter(([t]) => t === '.').map(([, x]) => x);
    const xb = b.calls.filter(([t]) => t === '.').map(([, x]) => x);
    expect(xa).toEqual(xb);
  });
  it('restores the text alignment', () => {
    const c = ctx();
    c.textAlign = 'center';
    fillTabular(c, '12', 0, 0, 'left');
    expect(c.textAlign).toBe('center');
  });
});
