/**
 * Drawing. Two layers: the BASE (grid, candles, volume, levels, live lines, axes, tags) redrawn when data or view
 * change, and the OVERLAY (crosshair and its tags) redrawn on pointer moves only. Both take a fully computed
 * scene (engine.ts) and draw exactly it: no value is computed here except pixel positions.
 *
 * Pixel snapping: 1 px lines are drawn on device-pixel centres (`crisp`), candle bodies on whole device pixels,
 * so the chart is sharp at any devicePixelRatio.
 */
import type { CandleStyle, ChartCandle, ChartLevel, ChartPrice } from './types';
import type { Palette } from './palette';
import { inkOn, withAlpha } from './palette';
import type { PriceRange, PriceTicks, TimeTick, View } from './scale';
import { priceY, slotX } from './scale';
import type { SeriesData } from './series';
import { slotOfTime } from './series';
import type { PlacedTag } from './levels';
import { liquidationZone } from './levels';
import { fillTabular, tabularWidth } from './text';
import { formatAxisTime, formatNumber } from './format';

export type Ctx = CanvasRenderingContext2D;

export type SceneFill = { readonly id: string; readonly slot: number; readonly price: number; readonly side: 'buy' | 'sell'; readonly liquidation: boolean };

export type Scene = {
  readonly width: number;
  readonly height: number;
  readonly dpr: number;
  /** Plot area: x in [0, plotW], y in [0, plotH]. Price axis right of it, time axis below. */
  readonly plotW: number;
  readonly plotH: number;
  readonly axisW: number;
  readonly timeH: number;
  readonly view: View;
  readonly range: PriceRange | null;
  readonly priceTicks: PriceTicks;
  readonly timeTicks: readonly TimeTick[];
  readonly series: SeriesData;
  /** Visible candle index range [i0, i1). */
  readonly i0: number;
  readonly i1: number;
  readonly volMax: number;
  /** True when the price range is the automatic one (the volume lane is then clear of candles). */
  readonly laneClear?: boolean;
  /** CSS px free between the newest candle and the price axis at this view; the level labels stand there when they fit. */
  readonly labelRoom?: number;
  /** Moving averages of the visible candles: values[j] belongs to candle i0 + j (NaN: none there). Display only. */
  readonly averages?: readonly { readonly period: number; readonly values: Float64Array }[];
  readonly palette: Palette;
  readonly candleStyle: CandleStyle;
  readonly priceDecimals: number;
  readonly mark: ChartPrice | null;
  readonly index: ChartPrice | null;
  readonly last: ChartCandle | null;
  readonly levels: readonly ChartLevel[];
  readonly tags: readonly PlacedTag[];
  readonly tagH: number;
  readonly fills: readonly SceneFill[];
  readonly stale: boolean;
  /** Font size of axis text, CSS px. */
  readonly fontPx: number;
};

/**
 * Share of the plot height that is the volume LANE: its own band under the candles, own scale, separated by a rule.
 * With the automatic price range the candles never enter it (engine.ts sets the range so the lowest low sits above
 * the lane) and no price tick is written beside it: a price label there would name a price no candle is drawn at.
 */
export const VOLUME_SHARE = 0.2;

const crisp = (v: number, dpr: number) => (Math.round(v * dpr - 0.5) + 0.5) / dpr;
const snap = (v: number, dpr: number) => Math.round(v * dpr) / dpr;

export function bodyWidth(spacing: number, dpr: number): number {
  // About 70 % of the slot, an odd number of device pixels so the wick sits in its middle.
  let w = Math.max(1, Math.floor(spacing * 0.7 * dpr));
  if (w % 2 === 0) w -= 1;
  return Math.max(1, w) / dpr;
}

export type LevelStyle = { color: string; dash: number[]; width: number };

export function levelStyle(level: ChartLevel, p: Palette): LevelStyle {
  switch (level.kind) {
    case 'liquidation':
      return { color: p.danger, dash: [], width: 1.5 };
    case 'position':
      return { color: level.side === 'buy' ? p.up : p.down, dash: [], width: 1 };
    case 'order':
      return { color: level.side === 'buy' ? p.up : p.down, dash: [6, 4], width: 1 };
    default:
      return { color: p.warn, dash: [2, 3, 8, 3], width: 1 };
  }
}

export type TagStyle = { fill: string; ink: string; line: string };

/** Fill and text colour of an axis tag, the text ink chosen for contrast on the fill. */
export function tagStyle(kind: PlacedTag['kind'], scene: Pick<Scene, 'palette' | 'levels' | 'last' | 'stale'>, id: string): TagStyle {
  const p = scene.palette;
  const solid = (fill: string): TagStyle => ({ fill, ink: inkOn(fill, p.bg, p.text), line: fill });
  if (kind === 'mark') return scene.stale ? { fill: p.surface, ink: p.text3, line: p.text3 } : solid(p.net);
  if (kind === 'index') return { fill: p.surface, ink: p.text2, line: p.text3 };
  if (kind === 'last') {
    if (scene.stale || !scene.last) return { fill: p.surface, ink: p.text3, line: p.text3 };
    return solid(scene.last.c >= scene.last.o ? p.up : p.down);
  }
  const level = scene.levels.find((l) => l.id === id);
  return level ? solid(levelStyle(level, p).color) : solid(p.text2);
}

function line(ctx: Ctx, x0: number, y0: number, x1: number, y1: number) {
  ctx.beginPath();
  ctx.moveTo(x0, y0);
  ctx.lineTo(x1, y1);
  ctx.stroke();
}

function roundRect(ctx: Ctx, x: number, y: number, w: number, h: number, r: number) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + w - r, y);
  ctx.arcTo(x + w, y, x + w, y + r, r);
  ctx.lineTo(x + w, y + h - r);
  ctx.arcTo(x + w, y + h, x + w - r, y + h, r);
  ctx.lineTo(x + r, y + h);
  ctx.arcTo(x, y + h, x, y + h - r, r);
  ctx.lineTo(x, y + r);
  ctx.arcTo(x, y, x + r, y, r);
  ctx.closePath();
}

export function font(scene: Pick<Scene, 'palette' | 'fontPx'>, weight = 450, px = scene.fontPx): string {
  return `${weight} ${px}px ${scene.palette.font}`;
}

/** The base layer. Returns counts of what was drawn (tests and the measurement page). */
export function drawBase(ctx: Ctx, s: Scene): { candles: number; volumes: number; levels: number; tags: number; fills: number } {
  const { dpr, palette: p, plotW, plotH, view } = s;
  const drawn = { candles: 0, volumes: 0, levels: 0, tags: 0, fills: 0 };
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, s.width, s.height);
  const range = s.range;
  const toY = (price: number) => (range ? priceY(price, range, 0, plotH) : NaN);

  // Grid.
  ctx.lineWidth = 1 / dpr;
  ctx.strokeStyle = withAlpha(p.grid, 0.7);
  ctx.setLineDash([]);
  const laneTop = s.laneClear && s.volMax > 0 ? plotH * (1 - VOLUME_SHARE) : plotH;
  if (range)
    for (const v of s.priceTicks.values) {
      const y = toY(v);
      if (y > laneTop) continue;
      line(ctx, 0, crisp(y, dpr), plotW, crisp(y, dpr));
    }
  for (const tick of s.timeTicks) {
    const x = crisp(slotX((tick.t - s.series.anchor) / s.series.intervalMs, view, plotW), dpr);
    if (x < 0 || x > plotW) continue;
    ctx.strokeStyle = withAlpha(p.grid, tick.rank === 'time' ? 0.55 : 0.95);
    line(ctx, x, 0, x, plotH);
  }

  ctx.save();
  ctx.beginPath();
  ctx.rect(0, 0, plotW, plotH);
  ctx.clip();

  // Liquidation zones under everything else.
  if (range) {
    for (const level of s.levels) {
      if (level.kind !== 'liquidation' || !Number.isFinite(level.price)) continue;
      const zone = liquidationZone(toY(level.price), level.side, 0, laneTop);
      if (!zone) continue;
      const g = ctx.createLinearGradient(0, zone[0], 0, zone[1]);
      const near = level.side === 'buy' ? 0 : 1;
      g.addColorStop(near, withAlpha(p.danger, 0.13));
      g.addColorStop(1 - near, withAlpha(p.danger, 0.02));
      ctx.fillStyle = g;
      ctx.fillRect(0, zone[0], plotW, zone[1] - zone[0]);
    }
  }

  const cs = s.series.candles;
  const slots = s.series.slots;
  const bw = bodyWidth(view.spacing, dpr);
  // Candles and volume are drawn in three colour groups (up, down, and the muted live candle when stale): one path
  // and one stroke per group for the wicks and hollow bodies, one fillStyle per group for the solid bodies. Measured
  // reason: a stroke per candle dominated the frame time under CPU throttling.
  const group = (i: number) => (s.stale && i === cs.length - 1 ? 2 : cs[i].c >= cs[i].o ? 0 : 1);
  const inks = [p.up, p.down, p.text3];
  // Volume (own scale, bottom band).
  if (s.volMax > 0) {
    const band = plotH * VOLUME_SHARE;
    const vw = Math.max(1 / dpr, snap(view.spacing * 0.8, dpr));
    const alphas = [0.34, 0.34, 0.14];
    if (s.laneClear) {
      // The rule that separates the lane, and its name.
      ctx.strokeStyle = withAlpha(p.grid, 0.95);
      ctx.lineWidth = 1 / dpr;
      line(ctx, 0, crisp(plotH - band, dpr), plotW, crisp(plotH - band, dpr));
      ctx.font = font(s, 500, s.fontPx - 1);
      ctx.textBaseline = 'top';
      ctx.fillStyle = p.text3;
      // At the left of its own lane: tags and level labels stop at the lane's top (engine.ts), so nothing covers it.
      fillTabular(ctx, 'Volume', 8, plotH - band + 5, 'left');
    }
    for (let g = 0; g < 3; g++) {
      ctx.fillStyle = withAlpha(inks[g], alphas[g]);
      for (let i = s.i0; i < s.i1; i++) {
        if (group(i) !== g) continue;
        const c = cs[i];
        if (!(c.v > 0)) continue;
        const h = Math.max(1 / dpr, (c.v / s.volMax) * (band - (s.laneClear ? s.fontPx + 8 : 0)));
        const x = snap(slotX(slots[i], view, plotW) - vw / 2, dpr);
        ctx.fillRect(x, snap(plotH - h, dpr), vw, plotH - snap(plotH - h, dpr));
        drawn.volumes++;
      }
    }
  }

  // Candles.
  if (range) {
    const thin = view.spacing < 3;
    ctx.lineWidth = 1 / dpr;
    for (let g = 0; g < 3; g++) {
      ctx.strokeStyle = inks[g];
      ctx.fillStyle = inks[g];
      ctx.beginPath();
      let solid = 0;
      for (let i = s.i0; i < s.i1; i++) {
        if (group(i) !== g) continue;
        const c = cs[i];
        const xc = crisp(slotX(slots[i], view, plotW), dpr);
        const yh = snap(toY(c.h), dpr);
        const yl = snap(toY(c.l), dpr);
        drawn.candles++;
        if (thin) {
          ctx.moveTo(xc, yh);
          ctx.lineTo(xc, Math.max(yl, yh + 1 / dpr));
          continue;
        }
        const yo = toY(c.o);
        const yc = toY(c.c);
        const top = snap(Math.min(yo, yc), dpr);
        const bottom = Math.max(snap(Math.max(yo, yc), dpr), top + 1 / dpr);
        // Wick: above and below the body only, so a hollow body stays empty.
        ctx.moveTo(xc, yh);
        ctx.lineTo(xc, top);
        ctx.moveTo(xc, bottom);
        ctx.lineTo(xc, Math.max(yl, bottom));
        const up = c.c >= c.o;
        if (up && s.candleStyle === 'hollow' && bw >= 3 / dpr && bottom - top >= 3 / dpr) {
          ctx.rect(snap(xc - bw / 2, dpr) + 0.5 / dpr, top + 0.5 / dpr, bw - 1 / dpr, bottom - top - 1 / dpr);
        } else solid++;
      }
      ctx.stroke();
      if (thin || !solid) continue;
      for (let i = s.i0; i < s.i1; i++) {
        if (group(i) !== g) continue;
        const c = cs[i];
        const up = c.c >= c.o;
        const yo = toY(c.o);
        const yc = toY(c.c);
        const top = snap(Math.min(yo, yc), dpr);
        const bottom = Math.max(snap(Math.max(yo, yc), dpr), top + 1 / dpr);
        if (up && s.candleStyle === 'hollow' && bw >= 3 / dpr && bottom - top >= 3 / dpr) continue;
        const xc = crisp(slotX(slots[i], view, plotW), dpr);
        ctx.fillRect(snap(xc - bw / 2, dpr), top, bw, bottom - top);
      }
    }
  }

  // Moving averages (display only): one line per period over the candles, broken wherever there is no value.
  if (range && s.averages?.length) {
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, plotW, laneTop);
    ctx.clip();
    ctx.lineWidth = 1.25;
    ctx.lineJoin = 'round';
    s.averages.forEach((a, n) => {
      ctx.strokeStyle = p.averages[n % p.averages.length];
      ctx.beginPath();
      let open = false;
      for (let i = s.i0; i < s.i1; i++) {
        const v = a.values[i - s.i0];
        if (!Number.isFinite(v) || (open && slots[i] - slots[i - 1] !== 1)) {
          open = false;
          if (!Number.isFinite(v)) continue;
        }
        const x = slotX(slots[i], view, plotW);
        const y = toY(v);
        if (open) ctx.lineTo(x, y);
        else ctx.moveTo(x, y);
        open = true;
      }
      ctx.stroke();
    });
    ctx.restore();
  }

  // Live lines: index (dotted), last (dotted, direction colour), mark (dashed lamp). None is drawn in the volume
  // lane: a price that far down is below the price pane (its tag is pinned at the pane's edge instead).
  const live = (price: number, color: string, dash: number[], alpha = 1) => {
    if (!range) return;
    const y = toY(price);
    if (y < 0 || y > laneTop) return;
    ctx.setLineDash(dash);
    ctx.strokeStyle = withAlpha(color, alpha);
    ctx.lineWidth = 1 / dpr;
    line(ctx, 0, crisp(y, dpr), plotW, crisp(y, dpr));
    ctx.setLineDash([]);
  };
  if (s.index) live(s.index.price, s.stale ? p.text3 : p.text2, [1, 3], 0.9);
  if (s.last) live(s.last.c, s.stale ? p.text3 : s.last.c >= s.last.o ? p.up : p.down, [1, 2], 0.8);
  if (s.mark) live(s.mark.price, s.stale ? p.text3 : p.net, [5, 4], 0.95);

  // Fills (under the level labels, so a marker never covers a label's text).
  for (const f of s.fills) {
    if (!range) break;
    const x = slotX(f.slot, view, plotW);
    const y = toY(f.price);
    if (x < -8 || x > plotW + 8 || y < -8 || y > plotH + 8) continue;
    const r = Math.max(3.5, Math.min(6, view.spacing * 0.45));
    ctx.fillStyle = f.liquidation ? p.danger : f.side === 'buy' ? p.up : p.down;
    ctx.strokeStyle = p.bg;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    if (f.liquidation) {
      ctx.moveTo(x, y - r);
      ctx.lineTo(x + r, y);
      ctx.lineTo(x, y + r);
      ctx.lineTo(x - r, y);
    } else if (f.side === 'buy') {
      ctx.moveTo(x, y - r);
      ctx.lineTo(x + r, y + r * 0.8);
      ctx.lineTo(x - r, y + r * 0.8);
    } else {
      ctx.moveTo(x, y + r);
      ctx.lineTo(x + r, y - r * 0.8);
      ctx.lineTo(x - r, y - r * 0.8);
    }
    ctx.closePath();
    ctx.stroke();
    ctx.fill();
    drawn.fills++;
  }
  // The user's levels: line across the plot and a label at the left.
  ctx.font = font(s, 500);
  ctx.textBaseline = 'middle';
  const tagById = new Map(s.tags.map((t) => [t.id, t]));
  // Where the labels stand. In the breathing room right of the newest candle when EVERY label fits there (with its
  // detail, or without it), so no label covers a candle; otherwise all at the left edge (a narrow plot, or a view the
  // user panned), over the oldest candles on screen. One side for all labels: they stay a column.
  // Three wordings, the fullest that fits the room for every label: with the detail, the label alone, the short one.
  const room = (s.labelRoom ?? 0) - 12;
  const widestOf = (mode: LabelMode) => {
    let w = 0;
    for (const level of s.levels) {
      const tag = tagById.get(level.id);
      if (tag && Number.isFinite(level.price)) w = Math.max(w, levelLabelWidth(ctx, s, level, tag, mode));
    }
    return w;
  };
  const fits = (['full', 'main', 'short'] as LabelMode[]).find((m) => {
    const w = widestOf(m);
    return w > 0 && w <= room;
  });
  const side: LabelSide = fits ? 'right' : 'left';
  const mode: LabelMode = fits ?? 'full';
  const widest = widestOf(mode);
  const elbowX = side === 'left' ? Math.min(8 + widest + 10, s.plotW - 12) : Math.max(12, s.plotW - 8 - widest - 10);
  for (const level of s.levels) {
    if (!range || !Number.isFinite(level.price)) continue;
    const st = levelStyle(level, p);
    const y = toY(level.price);
    const tag = tagById.get(level.id);
    if (y >= 0 && y <= laneTop) {
      ctx.setLineDash(st.dash);
      ctx.strokeStyle = st.color;
      ctx.lineWidth = st.width;
      line(ctx, 0, st.width === 1 ? crisp(y, dpr) : y, plotW, st.width === 1 ? crisp(y, dpr) : y);
      ctx.setLineDash([]);
    }
    if (tag) drawLevelLabel(ctx, s, level, st.color, tag, elbowX, side, mode);
    drawn.levels++;
  }

  ctx.restore();

  // Axes.
  const ax = plotW;
  ctx.fillStyle = p.bg;
  ctx.fillRect(ax, 0, s.axisW, s.height);
  ctx.fillRect(0, plotH, plotW, s.timeH);
  ctx.strokeStyle = p.axisLine;
  ctx.lineWidth = 1 / dpr;
  line(ctx, crisp(ax, dpr), 0, crisp(ax, dpr), plotH);
  line(ctx, 0, crisp(plotH, dpr), plotW, crisp(plotH, dpr));
  ctx.font = font(s);
  ctx.fillStyle = p.text3;
  ctx.textBaseline = 'middle';
  if (range) {
    for (const v of s.priceTicks.values) {
      const y = toY(v);
      if (y < s.fontPx / 2 || y > plotH - s.fontPx / 2) continue;
      if (y > laneTop - s.fontPx / 2) continue;
      // A tick label never shows half-covered under a tag.
      if (s.tags.some((t) => Math.abs(t.y - y) < s.tagH / 2 + s.fontPx / 2)) continue;
      fillTabular(ctx, formatNumber(v, s.priceTicks.decimals), ax + 8, y, 'left');
    }
  }
  ctx.textBaseline = 'middle';
  for (const tick of s.timeTicks) {
    const x = slotX((tick.t - s.series.anchor) / s.series.intervalMs, view, plotW);
    if (x < 24 || x > plotW - 24) continue;
    ctx.fillStyle = tick.rank === 'time' ? p.text3 : p.text2;
    ctx.font = font(s, tick.rank === 'time' ? 450 : 600);
    fillTabular(ctx, formatAxisTime(tick.t, tick.rank), x, plotH + s.timeH / 2, 'center');
  }

  // Axis tags.
  for (const tag of s.tags) {
    const text = tagText(s, tag);
    if (text === null) continue;
    drawAxisTag(ctx, s, tag, text);
    drawn.tags++;
  }
  return drawn;
}

/** Text of an axis tag: the app's exact text for levels, mark and index; the last close at the market's decimals. */
export function tagText(s: Pick<Scene, 'levels' | 'mark' | 'index' | 'last' | 'priceDecimals'>, tag: Pick<PlacedTag, 'id' | 'kind'>): string | null {
  switch (tag.kind) {
    case 'mark':
      return s.mark?.text ?? null;
    case 'index':
      return s.index?.text ?? null;
    case 'last':
      return s.last ? formatNumber(s.last.c, s.priceDecimals) : null;
    default:
      return s.levels.find((l) => l.id === tag.id)?.priceText ?? null;
  }
}

function drawAxisTag(ctx: Ctx, s: Scene, tag: PlacedTag, text: string) {
  const st = tagStyle(tag.kind, s, tag.id);
  const h = s.tagH;
  const x = s.plotW + 1;
  const w = s.axisW - 2;
  const y = tag.y - h / 2;
  // Name of a live price (the levels carry theirs in the label at the left).
  const name = tag.kind === 'mark' ? 'Mark' : tag.kind === 'index' ? 'Index' : tag.kind === 'last' ? 'Last' : null;
  if (name) {
    ctx.font = font(s, 600, s.fontPx - 1);
    ctx.textBaseline = 'middle';
    const nw = tabularWidth(ctx, name) + 10;
    ctx.fillStyle = withAlpha(s.palette.bg, 0.85);
    ctx.fillRect(s.plotW - nw - 2, tag.y - h / 2 + 3, nw, h - 6);
    ctx.fillStyle = st.fill === s.palette.surface ? st.ink : st.fill;
    fillTabular(ctx, name, s.plotW - 7, tag.y + 0.5, 'right');
  }
  // Leader from the price to a displaced tag.
  if (!tag.pinned && Math.abs(tag.y - tag.priceY) > 1) {
    ctx.strokeStyle = st.line;
    ctx.lineWidth = 1;
    line(ctx, s.plotW - 6, tag.priceY, x, tag.y);
  }
  ctx.fillStyle = st.fill;
  roundRect(ctx, x, y, w, h, 3);
  ctx.fill();
  if (st.fill === s.palette.surface) {
    ctx.strokeStyle = st.line;
    ctx.lineWidth = 1 / s.dpr;
    ctx.stroke();
  }
  ctx.fillStyle = st.ink;
  ctx.font = font(s, 600);
  ctx.textBaseline = 'middle';
  let tx = x + 7;
  if (tag.pinned) {
    // Arrow: the price is beyond this edge.
    const ay = tag.y;
    const up = tag.pinned === 'above';
    ctx.beginPath();
    ctx.moveTo(tx, ay + (up ? 3 : -3));
    ctx.lineTo(tx + 4, ay + (up ? -3 : 3));
    ctx.lineTo(tx + 8, ay + (up ? 3 : -3));
    ctx.closePath();
    ctx.fill();
    tx += 12;
  }
  fillTabular(ctx, text, tx, tag.y + 0.5, 'left');
}

/** Width of a level's left label box (shared by the drawing and the common leader column). */
export type LabelSide = 'left' | 'right';
/** Wording of the level labels: the label with its detail, the label alone, or the app's short label. */
export type LabelMode = 'full' | 'main' | 'short';

function labelMain(level: ChartLevel, tag: PlacedTag, mode: LabelMode): string {
  const words = mode === 'short' ? (level.short ?? level.label) : level.label;
  // A pinned level says on its label too that its price is beyond the edge.
  return tag.pinned === 'above' ? `\u2191 ${words}` : tag.pinned === 'below' ? `\u2193 ${words}` : words;
}

function levelLabelWidth(ctx: Ctx, s: Scene, level: ChartLevel, tag: PlacedTag, mode: LabelMode = 'full'): number {
  ctx.font = font(s, 600);
  const wMain = tabularWidth(ctx, labelMain(level, tag, mode));
  ctx.font = font(s, 500);
  const wDetail = mode === 'full' && level.detail ? tabularWidth(ctx, level.detail) + 8 : 0;
  return Math.min(s.plotW - 16, wMain + wDetail + 14);
}

function drawLevelLabel(ctx: Ctx, s: Scene, level: ChartLevel, color: string, tag: PlacedTag, elbowX: number, side: LabelSide = 'left', mode: LabelMode = 'full') {
  const p = s.palette;
  ctx.font = font(s, 600);
  const main = labelMain(level, tag, mode);
  const detail = mode === 'full' ? (level.detail ?? '') : '';
  const pad = 7;
  const wMain = tabularWidth(ctx, main);
  ctx.font = font(s, 500);
  const wDetail = detail ? tabularWidth(ctx, detail) + 8 : 0;
  const w = Math.min(s.plotW - 16, wMain + wDetail + pad * 2);
  const h = s.tagH;
  const x = side === 'left' ? 8 : s.plotW - 8 - w;
  const y = tag.y - h / 2;
  // A label pushed off its line (crowded levels) is tied to it by a leader, so it never names another line.
  if (!tag.pinned && Math.abs(tag.y - tag.priceY) > 1) {
    ctx.strokeStyle = color;
    ctx.lineWidth = 1;
    ctx.setLineDash([]);
    // All leaders turn at one column, in price order, so they never cross (mirrored when the labels stand at the right).
    if (side === 'left') {
      line(ctx, x + w, tag.y, elbowX, tag.y);
      line(ctx, elbowX, tag.y, elbowX + 8, tag.priceY);
    } else {
      line(ctx, x, tag.y, elbowX, tag.y);
      line(ctx, elbowX, tag.y, elbowX - 8, tag.priceY);
    }
  }
  ctx.fillStyle = withAlpha(p.bg, 0.92);
  roundRect(ctx, x, y, w, h, 3);
  ctx.fill();
  ctx.strokeStyle = color;
  ctx.lineWidth = 1;
  ctx.stroke();
  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, w, h);
  ctx.clip();
  ctx.textBaseline = 'middle';
  ctx.font = font(s, 600);
  ctx.fillStyle = p.text;
  fillTabular(ctx, main, x + pad, tag.y + 0.5, 'left');
  if (detail) {
    ctx.font = font(s, 500);
    ctx.fillStyle = level.detailTone === 'up' ? p.up : level.detailTone === 'down' ? p.down : p.text2;
    fillTabular(ctx, detail, x + pad + wMain + 8, tag.y + 0.5, 'left');
  }
  ctx.restore();
}

export type Crosshair = {
  readonly x: number;
  readonly y: number;
  readonly slot: number;
  readonly priceText: string | null;
  readonly timeText: string;
  /** Draw the "+" pick button next to the price tag. */
  readonly pick: boolean;
};

/** Box of the "+" pick button for a crosshair at y (hit testing and drawing share it). */
export function pickBox(s: Pick<Scene, 'plotW' | 'tagH'>, y: number): { x: number; y: number; w: number; h: number } {
  const size = s.tagH;
  return { x: s.plotW - size - 4, y: y - size / 2, w: size, h: size };
}

export function drawOverlay(ctx: Ctx, s: Scene, c: Crosshair | null) {
  const { dpr, palette: p } = s;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, s.width, s.height);
  if (!c) return;
  const x = crisp(slotX(c.slot, s.view, s.plotW), dpr);
  ctx.strokeStyle = withAlpha(p.text2, 0.75);
  ctx.lineWidth = 1 / dpr;
  ctx.setLineDash([3, 3]);
  if (x >= 0 && x <= s.plotW) line(ctx, x, 0, x, s.plotH);
  if (c.priceText !== null) line(ctx, 0, crisp(c.y, dpr), s.plotW, crisp(c.y, dpr));
  ctx.setLineDash([]);
  ctx.font = font(s, 600);
  ctx.textBaseline = 'middle';
  const fill = p.text;
  const ink = inkOn(fill, p.bg, p.text);
  if (c.priceText !== null) {
    ctx.fillStyle = fill;
    roundRect(ctx, s.plotW + 1, c.y - s.tagH / 2, s.axisW - 2, s.tagH, 3);
    ctx.fill();
    ctx.fillStyle = ink;
    fillTabular(ctx, c.priceText, s.plotW + 8, c.y + 0.5, 'left');
    if (c.pick) {
      const b = pickBox(s, c.y);
      ctx.fillStyle = fill;
      roundRect(ctx, b.x, b.y, b.w, b.h, 3);
      ctx.fill();
      ctx.strokeStyle = ink;
      ctx.lineWidth = 1.5;
      const m = b.w / 2;
      line(ctx, b.x + m, b.y + 5, b.x + m, b.y + b.h - 5);
      line(ctx, b.x + 5, b.y + m, b.x + b.w - 5, b.y + m);
    }
  }
  if (x >= 0 && x <= s.plotW) {
    ctx.font = font(s, 500);
    const w = tabularWidth(ctx, c.timeText) + 14;
    const tx = Math.max(0, Math.min(s.plotW - w, x - w / 2));
    ctx.fillStyle = fill;
    roundRect(ctx, tx, s.plotH + 2, w, s.timeH - 4, 3);
    ctx.fill();
    ctx.fillStyle = ink;
    fillTabular(ctx, c.timeText, tx + w / 2, s.plotH + s.timeH / 2, 'center');
  }
}

/** Fills of the user placed in slots (time inside the candle's interval). */
export function placeFills(series: SeriesData, fills: readonly { id: string; time: number; price: number; side: 'buy' | 'sell'; liquidation?: boolean }[]): SceneFill[] {
  const out: SceneFill[] = [];
  for (const f of fills) {
    if (!Number.isFinite(f.time) || !Number.isFinite(f.price)) continue;
    out.push({ id: f.id, slot: slotOfTime(series, f.time), price: f.price, side: f.side, liquidation: !!f.liquidation });
  }
  return out;
}
