/**
 * Colours of the chart, read from the page's design tokens (CSS custom properties) so the chart is the same stone
 * as the rest of the interface in both themes. Candle colours: `--chart-up` / `--chart-down` when the page sets
 * them and they reach 3:1 against the background (WCAG 2.2 SC 1.4.11, graphical objects); otherwise the chart's
 * own values below, checked by test (palette.test.ts) to reach 3:1 on the committed backgrounds of both themes.
 * Text the chart writes on a tag is checked against the tag's fill at 4.5:1 (SC 1.4.3) and switched to the
 * better of the two text inks when it would fall short.
 */

export type Theme = 'dark' | 'light';

export type Palette = {
  readonly theme: Theme;
  readonly bg: string;
  readonly surface: string;
  readonly grid: string;
  readonly axisLine: string;
  readonly text: string;
  readonly text2: string;
  readonly text3: string;
  readonly up: string;
  readonly down: string;
  readonly net: string;
  readonly warn: string;
  readonly danger: string;
  readonly ok: string;
  /** Inks of the moving-average lines, in the order of the periods. */
  readonly averages: readonly string[];
  readonly font: string;
};

/** The chart's own candle colours (used when the page gives none or one below 3:1). */
export const OWN_CANDLES: Readonly<Record<Theme, { up: string; down: string }>> = {
  // Green-blue and vermilion, apart in lightness as well as hue (contrast between them at least 1.4:1, so readers
  // with red-green colour blindness still tell them apart); hollow up candles carry the direction by shape too.
  // Both reach 4.5:1 on the background and the surface: the same colours write the readout's close and change.
  dark: { up: '#56d3a2', down: '#ec6f58' },
  light: { up: '#0b5e3e', down: '#c03a24' },
};

/**
 * The chart's own inks for the moving-average lines: violet, sky and sand, none of them a colour that means
 * something elsewhere on the chart (up, down, mark, stop, liquidation). Each reaches 3:1 on the committed
 * backgrounds of its theme (SC 1.4.11; palette.test.ts).
 */
export const OWN_AVERAGES: Readonly<Record<Theme, readonly string[]>> = {
  dark: ['#b9a2ff', '#7cc4f4', '#d9c9a3'],
  light: ['#5b3fb3', '#1c5f96', '#6b5a2e'],
};

/** Committed token values of the frontend (src/styles/tokens.css), the fallback when a variable is missing. */
export const TOKEN_FALLBACK: Readonly<Record<Theme, Omit<Palette, 'theme' | 'up' | 'down' | 'averages'>>> = {
  dark: {
    bg: '#0b0e0f',
    surface: '#111516',
    grid: '#252d30',
    axisLine: '#364145',
    text: '#eceae3',
    text2: '#a8b0ae',
    text3: '#8a9492',
    net: '#c8fa72',
    warn: '#e9b44c',
    danger: '#f59585',
    ok: '#7fd6a0',
    font: "'Inter Variable', Inter, system-ui, -apple-system, 'Segoe UI', sans-serif",
  },
  light: {
    bg: '#f2f0e9',
    surface: '#fbfaf6',
    grid: '#ddd9cc',
    axisLine: '#c4bfb0',
    text: '#14191a',
    text2: '#485250',
    text3: '#5d6765',
    net: '#486f00',
    warn: '#7d5000',
    danger: '#ad281f',
    ok: '#1a6640',
    font: "'Inter Variable', Inter, system-ui, -apple-system, 'Segoe UI', sans-serif",
  },
};

export type Rgb = [number, number, number];

/** Parses #rgb, #rrggbb, rgb(r g b) and rgb(r, g, b) (alpha ignored); null for anything else. */
export function parseColor(input: string): Rgb | null {
  const s = input.trim().toLowerCase();
  let m = /^#([0-9a-f]{3})$/.exec(s);
  if (m) return [0, 1, 2].map((i) => parseInt(m![1][i] + m![1][i], 16)) as Rgb;
  m = /^#([0-9a-f]{6})(?:[0-9a-f]{2})?$/.exec(s);
  if (m) return [0, 2, 4].map((i) => parseInt(m![1].slice(i, i + 2), 16)) as Rgb;
  m = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/.exec(s);
  if (m) return [Number(m[1]), Number(m[2]), Number(m[3])].map((v) => Math.max(0, Math.min(255, v))) as Rgb;
  return null;
}

/** WCAG 2 relative luminance. */
export function luminance([r, g, b]: Rgb): number {
  const ch = (v: number) => {
    const c = v / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b);
}

/** WCAG contrast ratio of two colours; 0 when either cannot be parsed. */
export function contrast(a: string, b: string): number {
  const ca = parseColor(a);
  const cb = parseColor(b);
  if (!ca || !cb) return 0;
  const la = luminance(ca);
  const lb = luminance(cb);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** The ink of `a` or `b` with the higher contrast on `fill`. */
export function inkOn(fill: string, a: string, b: string): string {
  return contrast(a, fill) >= contrast(b, fill) ? a : b;
}

/** `color` at `alpha` as rgb(r g b / a); the colour itself when it cannot be parsed. */
export function withAlpha(color: string, alpha: number): string {
  const c = parseColor(color);
  return c ? `rgb(${c[0]} ${c[1]} ${c[2]} / ${alpha})` : color;
}

type Reader = (name: string) => string;

/** Build the palette from a variable reader (getComputedStyle(el).getPropertyValue) for `theme`. */
export function readPalette(read: Reader, theme: Theme): Palette {
  const fb = TOKEN_FALLBACK[theme];
  const pick = (name: string, fallback: string) => {
    const v = read(name).trim();
    return v && parseColor(v) ? v : fallback;
  };
  const bg = pick('--bg', fb.bg);
  const candle = (name: string, own: string) => {
    const v = read(name).trim();
    return v && contrast(v, bg) >= 3 ? v : own;
  };
  const font = read('--font-ui').trim() || fb.font;
  return {
    theme,
    bg,
    surface: pick('--surface', fb.surface),
    grid: pick('--line', fb.grid),
    axisLine: pick('--line-strong', fb.axisLine),
    text: pick('--text', fb.text),
    text2: pick('--text-2', fb.text2),
    text3: pick('--text-3', fb.text3),
    net: pick('--net', fb.net),
    warn: pick('--warn', fb.warn),
    danger: pick('--danger', fb.danger),
    ok: pick('--ok', fb.ok),
    up: candle('--chart-up', OWN_CANDLES[theme].up),
    down: candle('--chart-down', OWN_CANDLES[theme].down),
    averages: OWN_AVERAGES[theme],
    font,
  };
}
