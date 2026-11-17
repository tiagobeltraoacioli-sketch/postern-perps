/**
 * The chart engine: holds the data and the view, turns input into view changes, and draws at most once per
 * animation frame however many updates arrive (a busy market sends hundreds of messages a second; the feed already
 * batches them per frame, and the engine coalesces whatever reaches it into one base redraw per frame).
 *
 * What may animate: only the VIEW (the jump back to the newest candle) and never under reduced motion. A price, a
 * candle or a level is drawn where the data says, the frame it arrives: never tweened.
 */
import type { ChartCandle, ChartFill, ChartFrameStats, ChartInterval, ChartLevel, ChartPrice, CandleStyle } from './types';
import { buildSeries, candleAt, extent, indexRange, slotTime, type SeriesData } from './series';
import {
  INTERVAL_MS,
  autoRange,
  clamp,
  clampView,
  defaultView,
  priceTicks,
  priceY,
  snapPrice,
  timeTicks,
  visibleSlots,
  xSlot,
  yPrice,
  zoomAt,
  type PriceRange,
  type View,
} from './scale';
import { layoutTags, type TagInput } from './levels';
import { drawBase, drawOverlay, font, pickBox, placeFills, type Crosshair, type Ctx, type Scene } from './render';
import { readPalette, type Palette, type Theme } from './palette';
import { formatCandleTime, formatNumber } from './format';
import { tabularWidth } from './text';

/** Process-wide frame statistics (all chart instances), for the performance page and the measurement harness. */
export const chartStats: ChartFrameStats = { baseFrames: 0, overlayFrames: 0, frameMs: [], updates: 0 };
const MAX_FRAME_SAMPLES = 20_000;

export function resetChartStats() {
  chartStats.baseFrames = 0;
  chartStats.overlayFrames = 0;
  chartStats.frameMs.length = 0;
  chartStats.updates = 0;
}

export type EngineData = {
  symbol: string;
  interval: ChartInterval;
  candles: readonly ChartCandle[];
  priceDecimals: number;
  sizeDecimals: number;
  tickSize?: number;
  mark: ChartPrice | null;
  index: ChartPrice | null;
  levels: readonly ChartLevel[];
  fills: readonly ChartFill[];
  stale: boolean;
  candleStyle: CandleStyle;
  wheelZoom: boolean;
  pickable: boolean;
};

export type HoverInfo = {
  /** The candle under the pointer (null: an empty slot or none), its slot's open time. */
  candle: ChartCandle | null;
  slotTime: number;
  /** The newest candle is shown when nothing is hovered. */
  hovering: boolean;
};

export type EngineHooks = {
  onHover?: (info: HoverInfo) => void;
  /** Whether the view follows the newest candle (false: a "latest" button is useful). */
  onFollowChange?: (following: boolean) => void;
  onReachStart?: (oldestT: number) => void;
  onPricePick?: (price: number) => void;
  /** The colours in use (the component writes the up and down colours into its readout). */
  onPalette?: (palette: Palette) => void;
};

export type Surface = {
  base: HTMLCanvasElement;
  overlay: HTMLCanvasElement;
  /** Element whose CSS variables give the palette. */
  styleRoot: Element;
};

export type EngineOptions = {
  getContext?: (canvas: HTMLCanvasElement) => Ctx | null;
  requestFrame?: (cb: (t: number) => void) => number;
  cancelFrame?: (id: number) => void;
  now?: () => number;
};

const TIME_AXIS_H = 26;
const TAG_H = 20;
const FONT_PX = 11.5;
const JUMP_MS = 260;

export class ChartEngine {
  private readonly baseCtx: Ctx | null;
  private readonly overlayCtx: Ctx | null;
  private readonly requestFrame: (cb: (t: number) => void) => number;
  private readonly cancelFrame: (id: number) => void;
  private readonly now: () => number;

  private data: EngineData | null = null;
  private series: SeriesData = buildSeries([], 60_000, null);
  private seriesKey = '';
  private candlesRef: readonly ChartCandle[] | null = null;
  private view: View = defaultView(0);
  private viewSet = false;
  private manualRange: PriceRange | null = null;
  private following = true;
  private reportedOldest: number | null = null;

  private width = 0;
  private height = 0;
  private dpr = 1;
  private palette: Palette;
  private theme: Theme = 'dark';
  reducedMotion = false;

  private frame: number | null = null;
  private baseDirty = true;
  private overlayDirty = true;
  private scene: Scene | null = null;
  private crosshair: { x: number; y: number; keyboard: boolean } | null = null;
  private drag: { x: number; y: number; view: View; range: PriceRange | null; onAxis: boolean } | null = null;
  private pinch: { d: number; view: View; cx: number } | null = null;
  private pointers = new Map<number, { x: number; y: number }>();
  private anim: { from: number; to: number; start: number } | null = null;
  private lastHover: string | null = null;
  private destroyed = false;
  /** Draw counts of the last base frame (tests, measurement page). */
  lastDrawn = { candles: 0, volumes: 0, levels: 0, tags: 0, fills: 0 };
  /** Ids of tags that did not fit the axis on the last frame (listed in the accessible summary anyway). */
  droppedTags: string[] = [];

  constructor(
    private readonly surface: Surface,
    private readonly hooks: EngineHooks = {},
    options: EngineOptions = {},
  ) {
    const get = options.getContext ?? ((c: HTMLCanvasElement) => c.getContext('2d'));
    this.baseCtx = get(surface.base);
    this.overlayCtx = get(surface.overlay);
    this.requestFrame = options.requestFrame ?? ((cb) => requestAnimationFrame(cb));
    this.cancelFrame = options.cancelFrame ?? ((id) => cancelAnimationFrame(id));
    this.now = options.now ?? (() => performance.now());
    this.palette = readPalette(() => '', 'dark');
  }

  // ------------------------------------------------------------ inputs from the component

  setData(d: EngineData) {
    chartStats.updates++;
    const key = `${d.symbol}|${d.interval}`;
    const intervalMs = INTERVAL_MS[d.interval];
    if (key !== this.seriesKey) {
      this.seriesKey = key;
      this.series = buildSeries(d.candles, intervalMs, null);
      this.candlesRef = d.candles;
      this.viewSet = false;
      this.manualRange = null;
      this.reportedOldest = null;
      this.anim = null;
    } else if (d.candles !== this.candlesRef) {
      const prevLast = this.lastSlot();
      this.series = buildSeries(d.candles, intervalMs, this.series.candles.length ? this.series.anchor : null);
      this.candlesRef = d.candles;
      const last = this.lastSlot();
      if (this.viewSet && this.following && prevLast !== null && last !== null && last > prevLast) {
        this.view = { ...this.view, right: this.view.right + (last - prevLast) };
        if (this.anim) this.anim = { ...this.anim, to: this.anim.to + (last - prevLast) };
      }
    }
    this.data = d;
    if (!this.viewSet && this.series.candles.length) {
      this.view = defaultView(this.lastSlot()!);
      this.viewSet = true;
      this.setFollowing(true);
    }
    this.invalidate(true);
  }

  setTheme(theme: Theme) {
    this.theme = theme;
    this.readColours();
  }

  /** Re-read the colours from the page's CSS variables (theme change). */
  readColours() {
    const style = getComputedStyle(this.surface.styleRoot);
    this.palette = readPalette((n) => style.getPropertyValue(n), this.theme);
    this.hooks.onPalette?.(this.palette);
    this.invalidate(true);
  }

  resize(width: number, height: number, dpr: number) {
    if (width === this.width && height === this.height && dpr === this.dpr) return;
    this.width = Math.max(0, width);
    this.height = Math.max(0, height);
    this.dpr = dpr > 0 ? dpr : 1;
    for (const c of [this.surface.base, this.surface.overlay]) {
      c.width = Math.max(1, Math.round(this.width * this.dpr));
      c.height = Math.max(1, Math.round(this.height * this.dpr));
    }
    this.invalidate(true);
  }

  destroy() {
    this.destroyed = true;
    if (this.frame !== null) this.cancelFrame(this.frame);
    this.frame = null;
  }

  // ------------------------------------------------------------ view helpers

  private lastSlot(): number | null {
    const s = this.series.slots;
    return s.length ? s[s.length - 1] : null;
  }

  private firstSlot(): number | null {
    return this.series.slots.length ? this.series.slots[0] : null;
  }

  private plotW() {
    return this.scene?.plotW ?? Math.max(0, this.width - 72);
  }

  private plotH() {
    return Math.max(0, this.height - TIME_AXIS_H);
  }

  private setView(v: View) {
    const first = this.firstSlot();
    const last = this.lastSlot();
    this.view = first !== null && last !== null ? clampView(v, this.plotW(), first, last) : v;
    this.setFollowing(last !== null && this.view.right >= last + 0.5);
    this.invalidate(true);
  }

  private setFollowing(f: boolean) {
    if (f !== this.following) {
      this.following = f;
      this.hooks.onFollowChange?.(f);
    }
  }

  isFollowing() {
    return this.following;
  }

  getView(): View {
    return this.view;
  }

  getSeries(): SeriesData {
    return this.series;
  }

  getScene(): Scene | null {
    return this.scene;
  }

  /** Back to the newest candle (animated unless reduced motion). */
  goToLatest() {
    const last = this.lastSlot();
    if (last === null) return;
    const target = defaultView(last, this.view.spacing).right;
    this.manualRange = null;
    if (this.reducedMotion) {
      this.anim = null;
      this.setView({ ...this.view, right: target });
    } else {
      this.anim = { from: this.view.right, to: target, start: this.now() };
      this.setFollowing(true);
      this.invalidate(true);
    }
  }

  goToOldest() {
    const first = this.firstSlot();
    if (first === null) return;
    this.anim = null;
    this.setView({ ...this.view, right: first + this.plotW() / this.view.spacing - 1 });
  }

  zoom(factor: number, x?: number) {
    this.anim = null;
    const w = this.plotW();
    this.setView(zoomAt(this.view, w, x ?? w, factor));
  }

  pan(slots: number) {
    this.anim = null;
    this.setView({ ...this.view, right: this.view.right + slots });
  }

  resetPriceScale() {
    this.manualRange = null;
    this.invalidate(true);
  }

  // ------------------------------------------------------------ pointer and keyboard

  pointerDown(id: number, x: number, y: number) {
    this.pointers.set(id, { x, y });
    if (this.pointers.size === 2) {
      const [a, b] = [...this.pointers.values()];
      this.pinch = { d: Math.hypot(a.x - b.x, a.y - b.y) || 1, view: this.view, cx: (a.x + b.x) / 2 };
      this.drag = null;
      return;
    }
    const scene = this.scene;
    if (scene && this.hooks.onPricePick && this.data?.pickable && this.crosshair && !this.crosshair.keyboard) {
      const b = pickBox(scene, this.crosshair.y);
      if (x >= b.x && x <= b.x + b.w && y >= b.y && y <= b.y + b.h) {
        this.pick();
        return;
      }
    }
    this.anim = null;
    this.drag = { x, y, view: this.view, range: this.currentRange(), onAxis: !!scene && x > scene.plotW };
  }

  pointerMove(id: number, x: number, y: number) {
    if (this.pointers.has(id)) this.pointers.set(id, { x, y });
    if (this.pinch && this.pointers.size === 2) {
      const [a, b] = [...this.pointers.values()];
      const d = Math.hypot(a.x - b.x, a.y - b.y) || 1;
      this.setView(zoomAt(this.pinch.view, this.plotW(), this.pinch.cx, d / this.pinch.d));
      return;
    }
    if (this.drag) {
      const dx = x - this.drag.x;
      const dy = y - this.drag.y;
      if (this.drag.onAxis && this.drag.range) {
        // Drag on the price axis: stretch the price scale about its centre.
        const r = this.drag.range;
        const mid = (r.max + r.min) / 2;
        const half = ((r.max - r.min) / 2) * Math.exp(dy * 0.006);
        this.manualRange = { min: mid - half, max: mid + half };
        this.invalidate(true);
      } else {
        this.setView({ ...this.drag.view, right: this.drag.view.right - dx / this.drag.view.spacing });
        if (this.manualRange && this.drag.range) {
          const r = this.drag.range;
          const per = (r.max - r.min) / Math.max(1, this.plotH());
          this.manualRange = { min: r.min + dy * per, max: r.max + dy * per };
        }
      }
    }
    this.crosshair = { x, y, keyboard: false };
    this.invalidate(false);
  }

  pointerUp(id: number) {
    this.pointers.delete(id);
    if (this.pointers.size < 2) this.pinch = null;
    if (this.pointers.size === 0) this.drag = null;
  }

  pointerLeave() {
    this.pointers.clear();
    this.drag = null;
    this.pinch = null;
    this.crosshair = null;
    this.invalidate(false);
  }

  doubleClick(x: number) {
    if (this.scene && x > this.scene.plotW) this.resetPriceScale();
    else this.goToLatest();
  }

  /**
   * Wheel: returns true when the chart used the event (the caller then prevents the page from scrolling).
   * Vertical wheel zooms (unless wheelZoom is false), ctrl+wheel (trackpad pinch) always zooms, horizontal pans.
   */
  wheel(x: number, deltaX: number, deltaY: number, ctrl: boolean): boolean {
    const horizontal = Math.abs(deltaX) > Math.abs(deltaY);
    if (horizontal) {
      this.pan(deltaX / this.view.spacing);
      return true;
    }
    if (!ctrl && this.data && !this.data.wheelZoom) return false;
    const factor = Math.exp(-clamp(deltaY, -200, 200) * (ctrl ? 0.01 : 0.0025));
    this.zoom(factor, Math.min(x, this.plotW()));
    return true;
  }

  /** Keyboard control of the focused plot; returns true when the key was used. */
  key(key: string, shift: boolean): boolean {
    const scene = this.scene;
    switch (key) {
      case 'ArrowLeft':
        this.pan(-(shift ? 10 : 1));
        break;
      case 'ArrowRight':
        this.pan(shift ? 10 : 1);
        break;
      case '+':
      case '=':
        this.zoom(1.25);
        break;
      case '-':
      case '_':
        this.zoom(0.8);
        break;
      case 'Home':
        this.goToOldest();
        break;
      case 'End':
        this.goToLatest();
        break;
      case 'ArrowUp':
      case 'ArrowDown': {
        if (!scene || !scene.range || !this.data) return false;
        const step = this.data.tickSize && this.data.tickSize > 0 ? this.data.tickSize : 10 ** -this.data.priceDecimals;
        const cur = this.crosshair ? yPrice(this.crosshair.y, scene.range, 0, scene.plotH) : (this.series.candles.at(-1)?.c ?? (scene.range.max + scene.range.min) / 2);
        const pixelsPerStep = (step / (scene.range.max - scene.range.min)) * scene.plotH;
        const steps = Math.max(1, Math.ceil(2 / Math.max(pixelsPerStep, 1e-9))) * (shift ? 10 : 1);
        const next = snapPrice(cur + (key === 'ArrowUp' ? 1 : -1) * steps * step, this.data.tickSize, this.data.priceDecimals);
        const y = clamp(priceY(next, scene.range, 0, scene.plotH), 0, scene.plotH);
        const last = this.lastSlot();
        const x = this.crosshair?.x ?? (last !== null ? scene.plotW - (this.view.right - last) * this.view.spacing : scene.plotW / 2);
        this.crosshair = { x, y, keyboard: true };
        this.invalidate(false);
        break;
      }
      case 'Enter':
        if (!this.crosshair) return false;
        this.pick();
        break;
      case 'Escape':
        if (!this.crosshair) return false;
        this.crosshair = null;
        this.invalidate(false);
        break;
      default:
        return false;
    }
    return true;
  }

  private pick() {
    const scene = this.scene;
    if (!scene || !scene.range || !this.crosshair || !this.data || !this.hooks.onPricePick) return;
    const raw = yPrice(this.crosshair.y, scene.range, 0, scene.plotH);
    this.hooks.onPricePick(snapPrice(raw, this.data.tickSize, this.data.priceDecimals));
  }

  // ------------------------------------------------------------ frames

  invalidate(base: boolean) {
    if (base) this.baseDirty = true;
    this.overlayDirty = true;
    if (this.frame === null && !this.destroyed) this.frame = this.requestFrame(this.onFrame);
  }

  private onFrame = () => {
    this.frame = null;
    if (this.destroyed) return;
    const t0 = this.now();
    if (this.anim) {
      const k = clamp((t0 - this.anim.start) / JUMP_MS, 0, 1);
      const e = 1 - (1 - k) ** 3;
      this.view = { ...this.view, right: this.anim.from + (this.anim.to - this.anim.from) * e };
      this.baseDirty = true;
      if (k >= 1) {
        this.anim = null;
        this.setView(this.view);
      } else this.invalidate(true);
    }
    let drew = false;
    if (this.baseDirty) {
      this.baseDirty = false;
      this.scene = this.buildScene();
      if (this.baseCtx && this.scene) this.lastDrawn = drawBase(this.baseCtx, this.scene);
      chartStats.baseFrames++;
      drew = true;
      this.checkReachStart();
    }
    if (this.overlayDirty) {
      this.overlayDirty = false;
      const c = this.buildCrosshair();
      if (this.overlayCtx && this.scene) drawOverlay(this.overlayCtx, this.scene, c);
      chartStats.overlayFrames++;
      drew = true;
      this.emitHover(c);
    }
    if (drew) {
      chartStats.frameMs.push(this.now() - t0);
      if (chartStats.frameMs.length > MAX_FRAME_SAMPLES) chartStats.frameMs.splice(0, chartStats.frameMs.length - MAX_FRAME_SAMPLES);
    }
  };

  /** Run pending frame work now (tests and the measurement harness). */
  flush() {
    if (this.frame !== null) {
      this.cancelFrame(this.frame);
      this.onFrame();
    }
  }

  private currentRange(): PriceRange | null {
    return this.scene?.range ?? null;
  }

  private checkReachStart() {
    const scene = this.scene;
    const first = this.firstSlot();
    if (!scene || first === null || !this.hooks.onReachStart) return;
    const [s0] = visibleSlots(this.view, scene.plotW);
    if (s0 <= first + 2) {
      const oldest = this.series.candles[0].t;
      if (this.reportedOldest !== oldest) {
        this.reportedOldest = oldest;
        this.hooks.onReachStart(oldest);
      }
    }
  }

  private buildScene(): Scene | null {
    const d = this.data;
    if (!d || this.width <= 0 || this.height <= 0) return null;
    const ctx = this.baseCtx;
    const fontPx = FONT_PX;
    const plotH = this.plotH();
    const base = { palette: this.palette, fontPx };
    // Axis width from the widest text it will hold.
    let axisW = 64;
    if (ctx) {
      ctx.font = font(base, 600);
      const texts = [d.mark?.text, d.index?.text, ...d.levels.map((l) => l.priceText)];
      const lastC = this.series.candles.at(-1);
      if (lastC) texts.push(formatNumber(lastC.c, d.priceDecimals));
      let w = 0;
      for (const t of texts) if (t) w = Math.max(w, tabularWidth(ctx, t));
      axisW = clamp(Math.ceil(w + 12 + 12 + 2), 64, 180);
    }
    const plotW = Math.max(0, this.width - axisW);
    const first = this.firstSlot();
    const last = this.lastSlot();
    if (first !== null && last !== null && !this.anim) this.view = clampView(this.view, plotW, first, last);
    const view = this.view;
    const [s0, s1] = visibleSlots(view, plotW);
    const [i0, i1] = indexRange(this.series, s0, s1);
    const ext = extent(this.series, i0, i1);
    let range: PriceRange | null = this.manualRange;
    if (!range) {
      const raw = autoRange(ext.lo, ext.hi, [d.mark?.price, d.index?.price], 0);
      if (raw) {
        const span = raw.max - raw.min;
        // Room under the lows for the volume band, a little above the highs.
        range = { min: raw.min - span * 0.3, max: raw.max + span * 0.08 };
      } else if (d.mark || d.index) {
        const p = (d.mark ?? d.index)!.price;
        range = autoRange(p, p, [], 0.08);
      }
    }
    const pt = range ? priceTicks(range, plotH, d.priceDecimals, d.tickSize) : { step: 0, values: [], decimals: 0 };
    const intervalMs = this.series.intervalMs;
    const tStart = slotTime(this.series, s0);
    const tEnd = slotTime(this.series, s1);
    const tt = timeTicks(tStart, tEnd, view.spacing / intervalMs);
    const lastCandle = this.series.candles.at(-1) ?? null;
    const tagInputs: TagInput[] = [];
    for (const l of d.levels) if (Number.isFinite(l.price)) tagInputs.push({ id: l.id, kind: l.kind, price: l.price });
    if (d.mark) tagInputs.push({ id: '@mark', kind: 'mark', price: d.mark.price });
    if (lastCandle) tagInputs.push({ id: '@last', kind: 'last', price: lastCandle.c });
    if (d.index) tagInputs.push({ id: '@index', kind: 'index', price: d.index.price });
    const tags = range ? layoutTags(tagInputs, (p) => priceY(p, range!, 0, plotH), 0, plotH, TAG_H) : { placed: [], dropped: [] };
    // Live tags (mark, last, index) are not pinned: off screen they are simply not shown on the axis.
    const placed = tags.placed.filter((t) => !(t.pinned && (t.kind === 'mark' || t.kind === 'last' || t.kind === 'index')));
    this.droppedTags = tags.dropped;
    return {
      width: this.width,
      height: this.height,
      dpr: this.dpr,
      plotW,
      plotH,
      axisW,
      timeH: TIME_AXIS_H,
      view,
      range,
      priceTicks: pt,
      timeTicks: tt,
      series: this.series,
      i0,
      i1,
      volMax: ext.vMax,
      palette: this.palette,
      candleStyle: d.candleStyle,
      priceDecimals: d.priceDecimals,
      mark: d.mark,
      index: d.index,
      last: lastCandle,
      levels: d.levels,
      tags: placed,
      tagH: TAG_H,
      fills: placeFills(this.series, d.fills),
      stale: d.stale,
      fontPx,
    };
  }

  private buildCrosshair(): Crosshair | null {
    const s = this.scene;
    const c = this.crosshair;
    const d = this.data;
    if (!s || !c || !d) return null;
    if (c.x < 0 || c.y < 0 || c.y > s.plotH + s.timeH) return null;
    const slot = Math.round(xSlot(Math.min(c.x, s.plotW), s.view, s.plotW));
    const inPlot = c.y <= s.plotH && s.range !== null;
    const price = inPlot ? snapPrice(yPrice(c.y, s.range!, 0, s.plotH), d.tickSize, d.priceDecimals) : null;
    const y = price !== null ? priceY(price, s.range!, 0, s.plotH) : c.y;
    return {
      x: c.x,
      y,
      slot,
      priceText: price !== null ? formatNumber(price, d.priceDecimals) : null,
      timeText: formatCandleTime(slotTime(this.series, slot), this.series.intervalMs),
      pick: !!this.hooks.onPricePick && d.pickable && price !== null && !c.keyboard,
    };
  }

  private emitHover(c: Crosshair | null) {
    if (!this.hooks.onHover) return;
    const candle = c ? candleAt(this.series, c.slot) : (this.series.candles.at(-1) ?? null);
    const t = c ? slotTime(this.series, c.slot) : (candle?.t ?? 0);
    const key = `${c ? 1 : 0}|${t}|${candle ? `${candle.o},${candle.h},${candle.l},${candle.c},${candle.v}` : '-'}`;
    if (key === this.lastHover) return;
    this.lastHover = key;
    this.hooks.onHover({ candle, slotTime: t, hovering: !!c });
  }
}
