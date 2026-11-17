/**
 * Tabular figures on canvas. The 2D context cannot switch on OpenType `tnum`, so digits are drawn one by one on a
 * fixed advance (the widest digit of the font): a price written under another lines up digit for digit, and a
 * live price does not jitter as its digits change. Other characters keep their own width.
 */

export type TextCtx = Pick<CanvasRenderingContext2D, 'measureText' | 'fillText' | 'font' | 'textAlign' | 'textBaseline'>;

const DIGITS = '0123456789';
type Metrics = { digit: number; own: number[]; widths: Map<string, number> };
const cache = new Map<string, Metrics>();

function metrics(ctx: TextCtx) {
  const key = ctx.font;
  let m = cache.get(key);
  if (!m) {
    const own = [...DIGITS].map((d) => ctx.measureText(d).width);
    m = { digit: Math.max(...own), own, widths: new Map() };
    if (cache.size > 64) cache.clear();
    cache.set(key, m);
  }
  return m;
}

function charWidth(ctx: TextCtx, m: Metrics, ch: string): number {
  if (ch >= '0' && ch <= '9') return m.digit;
  let w = m.widths.get(ch);
  if (w === undefined) {
    w = ctx.measureText(ch).width;
    m.widths.set(ch, w);
  }
  return w;
}

/** Width of `text` drawn with tabular digits in the context's current font. */
export function tabularWidth(ctx: TextCtx, text: string): number {
  const m = metrics(ctx);
  let w = 0;
  for (const ch of text) w += charWidth(ctx, m, ch);
  return w;
}

/**
 * Draw `text` with tabular digits at (x, y); `align` is where x lies. Runs of non-digits are drawn in one call
 * (kerning inside words is kept); each digit is centred in its fixed cell. Returns the drawn width.
 */
export function fillTabular(ctx: TextCtx, text: string, x: number, y: number, align: 'left' | 'right' | 'center' = 'left'): number {
  const m = metrics(ctx);
  const width = tabularWidth(ctx, text);
  let cx = align === 'left' ? x : align === 'right' ? x - width : x - width / 2;
  const saved = ctx.textAlign;
  ctx.textAlign = 'left';
  let run = '';
  let runX = cx;
  const flush = () => {
    if (run) ctx.fillText(run, runX, y);
    run = '';
  };
  for (const ch of text) {
    const w = charWidth(ctx, m, ch);
    if (ch >= '0' && ch <= '9') {
      flush();
      ctx.fillText(ch, cx + (w - m.own[ch.charCodeAt(0) - 48]) / 2, y);
      runX = cx + w;
    } else {
      if (!run) runX = cx;
      run += ch;
    }
    cx += w;
  }
  flush();
  ctx.textAlign = saved;
  return width;
}

/** Test hook: forget measured fonts. */
export function resetTextCache() {
  cache.clear();
}
