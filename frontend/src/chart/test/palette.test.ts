import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { OWN_CANDLES, TOKEN_FALLBACK, contrast, inkOn, parseColor, readPalette } from '../palette';

describe('colour arithmetic', () => {
  it('parses the forms the tokens use', () => {
    expect(parseColor('#fff')).toEqual([255, 255, 255]);
    expect(parseColor(' #0b0e0f ')).toEqual([11, 14, 15]);
    expect(parseColor('rgb(1 2 3 / 0.5)')).toEqual([1, 2, 3]);
    expect(parseColor('rgb(1, 2, 3)')).toEqual([1, 2, 3]);
    expect(parseColor('var(--x)')).toBeNull();
  });
  it('WCAG contrast', () => {
    expect(contrast('#000', '#fff')).toBeCloseTo(21, 5);
    expect(contrast('#777', '#777')).toBeCloseTo(1, 9);
    expect(contrast('nope', '#fff')).toBe(0);
  });
});

describe('the chart colours reach WCAG AA on both committed themes', () => {
  for (const theme of ['dark', 'light'] as const) {
    const t = TOKEN_FALLBACK[theme];
    it(`${theme}: candle colours 4.5:1 on the background and the surface (graphics need 3:1; they also write text)`, () => {
      for (const c of [OWN_CANDLES[theme].up, OWN_CANDLES[theme].down]) {
        expect(contrast(c, t.bg)).toBeGreaterThanOrEqual(4.5);
        expect(contrast(c, t.surface)).toBeGreaterThanOrEqual(4.5);
      }
    });
    it(`${theme}: up and down are distinguishable by lightness too (contrast between them)`, () => {
      // Not a WCAG criterion: red-green colour-blind readers separate them by lightness. Hollow up candles also
      // carry the direction by shape.
      expect(contrast(OWN_CANDLES[theme].up, OWN_CANDLES[theme].down)).toBeGreaterThanOrEqual(1.4);
    });
    it(`${theme}: axis text 4.5:1 on the background`, () => {
      for (const c of [t.text, t.text2, t.text3]) expect(contrast(c, t.bg)).toBeGreaterThanOrEqual(4.5);
    });
    it(`${theme}: text on every tag fill reaches 4.5:1 with the chosen ink`, () => {
      const p = readPalette(() => '', theme);
      for (const fill of [p.up, p.down, p.net, p.danger, p.warn, p.text, p.text2]) {
        const ink = inkOn(fill, p.bg, p.text);
        expect(contrast(ink, fill), `${fill}`).toBeGreaterThanOrEqual(4.5);
      }
    });
  }
  it('the fallbacks equal the committed tokens of the frontend (when the file is present)', () => {
    let css = '';
    try {
      css = readFileSync(new URL('../../styles/tokens.css', import.meta.url), 'utf8');
    } catch {
      return; // the chart branch alone has no tokens file; the integrated tree runs this check
    }
    const dark = css.slice(css.indexOf(":root[data-theme='dark']"), css.indexOf('/* ---------- Light'));
    const light = css.slice(css.indexOf(":root[data-theme='light'] {"));
    const val = (block: string, name: string) => new RegExp(`${name}:\\s*(#[0-9a-f]{6})`).exec(block)?.[1];
    for (const [block, fb] of [[dark, TOKEN_FALLBACK.dark], [light, TOKEN_FALLBACK.light]] as const) {
      expect(val(block, '--bg')).toBe(fb.bg);
      expect(val(block, '--surface')).toBe(fb.surface);
      expect(val(block, '--text-3')).toBe(fb.text3);
      expect(val(block, '--danger')).toBe(fb.danger);
    }
  });
});

describe('reading the page', () => {
  it('takes the page variables, refuses a candle colour below 3:1', () => {
    const vars: Record<string, string> = { '--bg': '#0b0e0f', '--chart-up': '#111111', '--chart-down': '#ff8080', '--net': '#c8fa72' };
    const p = readPalette((n) => vars[n] ?? '', 'dark');
    expect(p.up).toBe(OWN_CANDLES.dark.up);
    expect(p.down).toBe('#ff8080');
    expect(p.net).toBe('#c8fa72');
  });
  it('a missing or unparsable variable falls back to the committed token', () => {
    const p = readPalette((n) => (n === '--text' ? 'var(--other)' : ''), 'light');
    expect(p.text).toBe(TOKEN_FALLBACK.light.text);
  });
});
