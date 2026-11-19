/**
 * Depth chart: the cumulative size resting on each side of the book, as two stepped areas that meet at the mid
 * price. It draws only the levels the exchange sent (the same book the Order book tab lists), over a price window
 * that is the same distance on both sides of the mid, so the two areas can be compared by eye. The areas are
 * geometry; every figure in the readout is formatted from the book's integers. Nothing is submitted from here.
 */
import { useMemo, useState, type KeyboardEvent, type PointerEvent } from 'react';
import { feed } from '../state/session';
import { useStore } from '../lib/store';
import { formatPpm, formatPx, formatSize, formatUsd, DASH } from '../lib/format';
import { mulDiv } from '../lib/fixed';
import { notional, type Level } from '../lib/calc';
import type { Market } from '../lib/types';

export type DepthPoint = { px: bigint; size: bigint; cum: bigint; usd: bigint };
export type DepthModel = {
  mid: bigint | null;
  /** Price window: [lo, hi], the same distance on both sides of the mid when both sides have levels. */
  lo: bigint;
  hi: bigint;
  /** Bids from the best (highest) down, asks from the best (lowest) up, inside the window. */
  bids: DepthPoint[];
  asks: DepthPoint[];
  max: bigint;
};

/**
 * The model of the picture. `bids` come best first (descending), `asks` best first (ascending), as the feed holds
 * them. The window reaches as far from the mid as the SHALLOWER side's last level, so neither area ends in an empty
 * stretch that would read as "no liquidity" where the exchange simply sent no more levels. Pure (tested).
 */
export function depthModel(bids: readonly Level[], asks: readonly Level[]): DepthModel {
  const side = (levels: readonly Level[], keep: (px: bigint) => boolean): DepthPoint[] => {
    const out: DepthPoint[] = [];
    let cum = 0n;
    let usd = 0n;
    for (const l of levels) {
      if (!keep(l.px)) break;
      cum += l.size;
      usd += notional(l.size, l.px);
      out.push({ px: l.px, size: l.size, cum, usd });
    }
    return out;
  };
  if (!bids.length && !asks.length) return { mid: null, lo: 0n, hi: 0n, bids: [], asks: [], max: 0n };
  if (!bids.length || !asks.length) {
    const only = bids.length ? bids : asks;
    const a = only[0].px;
    const b = only[only.length - 1].px;
    const points = side(only, () => true);
    return { mid: null, lo: a < b ? a : b, hi: a < b ? b : a, bids: bids.length ? points : [], asks: asks.length ? points : [], max: points[points.length - 1].cum };
  }
  const mid = (bids[0].px + asks[0].px) / 2n;
  const down = mid - bids[bids.length - 1].px;
  const up = asks[asks.length - 1].px - mid;
  const half = down < up ? down : up;
  const lo = mid - half;
  const hi = mid + half;
  const b = side(bids, (px) => px >= lo);
  const a = side(asks, (px) => px <= hi);
  const top = (p: DepthPoint[]) => (p.length ? p[p.length - 1].cum : 0n);
  return { mid, lo, hi, bids: b, asks: a, max: top(b) > top(a) ? top(b) : top(a) };
}

const W = 1000;
const H = 1000;

/** The stepped outline of one side as an SVG path in the 1000 x 1000 box (y grows downward). Pure (tested). */
export function depthPath(points: readonly DepthPoint[], model: Pick<DepthModel, 'lo' | 'hi' | 'max'>, edge: 'left' | 'right'): string {
  if (!points.length || model.hi <= model.lo || model.max <= 0n) return '';
  const x = (px: bigint) => round((Number(px - model.lo) / Number(model.hi - model.lo)) * W);
  const y = (cum: bigint) => round(H - (Number(cum) / Number(model.max)) * H * 0.92);
  let d = `M${x(points[0].px)} ${H}`;
  for (let i = 0; i < points.length; i++) {
    d += ` V${y(points[i].cum)}`;
    d += ` H${i + 1 < points.length ? x(points[i + 1].px) : edge === 'left' ? 0 : W}`;
  }
  return `${d} V${H} Z`;
}
const round = (v: number) => Math.round(v * 10) / 10;

type Pick_ = { side: 'bid' | 'ask'; i: number };

export function DepthChart({ market }: { market: Market }) {
  const book = useStore(feed.book);
  const connection = useStore(feed.connection);
  const model = useMemo(() => depthModel(book.bids, book.asks), [book]);
  const [at, setAt] = useState<Pick_ | null>(null);
  const stale = !book.ready || connection.state !== 'open';
  const empty = book.ready && !model.bids.length && !model.asks.length;
  const span = model.hi - model.lo;
  const frac = (px: bigint) => (span > 0n ? Number(px - model.lo) / Number(span) : 0.5);
  const point = at ? (at.side === 'bid' ? model.bids : model.asks)[at.i] : undefined;

  const onMove = (e: PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    if (!(r.width > 0) || span <= 0n) return;
    const f = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    const target = model.lo + (span * BigInt(Math.round(f * 10_000))) / 10_000n;
    // Bids: the deepest level still at or above the pointed price; asks: the deepest still at or below it.
    let i = -1;
    if (model.mid !== null && target <= model.mid) {
      for (let k = 0; k < model.bids.length && model.bids[k].px >= target; k++) i = k;
      setAt(i >= 0 ? { side: 'bid', i } : model.bids.length ? { side: 'bid', i: 0 } : null);
    } else {
      for (let k = 0; k < model.asks.length && model.asks[k].px <= target; k++) i = k;
      setAt(i >= 0 ? { side: 'ask', i } : model.asks.length ? { side: 'ask', i: 0 } : null);
    }
  };
  // Arrow keys walk the levels from the lowest bid to the highest ask.
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight' && e.key !== 'Escape') return;
    e.preventDefault();
    if (e.key === 'Escape') return setAt(null);
    const order: Pick_[] = [...model.bids.map((_, i) => ({ side: 'bid' as const, i })).reverse(), ...model.asks.map((_, i) => ({ side: 'ask' as const, i }))];
    if (!order.length) return;
    const now = at ? order.findIndex((o) => o.side === at.side && o.i === at.i) : -1;
    const next = now < 0 ? (e.key === 'ArrowRight' ? model.bids.length : Math.max(0, model.bids.length - 1)) : now + (e.key === 'ArrowRight' ? 1 : -1);
    setAt(order[Math.min(order.length - 1, Math.max(0, next))]);
  };

  const dp = market.priceDecimals;
  const distance = point && model.mid ? mulDiv(point.px > model.mid ? point.px - model.mid : model.mid - point.px, 1_000_000n, model.mid, 'ceil') : null;
  return (
    <section className={`panel depth ${stale ? 'is-stale' : ''}`} aria-label="Depth chart">
      <header className="panel-head">
        <h2>Depth</h2>
        <span className="depth-max num" title="Largest cumulative size in view">
          {model.max > 0n ? `${formatSize(model.max, market.sizeDecimals)} ${market.base}` : DASH}
        </span>
      </header>
      <div
        className="depth-plot"
        tabIndex={0}
        role="group"
        aria-label={`Cumulative book depth of ${market.symbol}: ${model.bids.length} bid and ${model.asks.length} ask levels. Arrow keys read the levels.`}
        onPointerMove={onMove}
        onPointerLeave={() => setAt(null)}
        onKeyDown={onKey}
        onBlur={() => setAt(null)}
      >
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" aria-hidden="true" focusable="false">
          <path className="depth-area depth-bid" d={depthPath(model.bids, model, 'left')} />
          <path className="depth-area depth-ask" d={depthPath(model.asks, model, 'right')} />
        </svg>
        {model.mid !== null && <span className="depth-mid" style={{ left: `${frac(model.mid) * 100}%` }} aria-hidden="true" />}
        {point && <span className={`depth-cursor depth-cursor-${at!.side}`} style={{ left: `${frac(point.px) * 100}%` }} aria-hidden="true" />}
        {empty && <p className="book-empty">No resting orders in this market yet.</p>}
      </div>
      <div className="depth-axis num" aria-hidden="true">
        <span>{span > 0n ? formatPx(model.lo, dp) : DASH}</span>
        <span className="depth-axis-mid">{model.mid !== null ? formatPx(model.mid, dp) : DASH}</span>
        <span>{span > 0n ? formatPx(model.hi, dp) : DASH}</span>
      </div>
      <p className="depth-readout" aria-live="polite">
        {point ? (
          <>
            <span className={at!.side === 'bid' ? 'up' : 'down'}>
              {at!.side === 'bid' ? 'Bids down to' : 'Asks up to'} <span className="num">{formatPx(point.px, dp)}</span>
            </span>
            <span className="num">
              {formatSize(point.cum, market.sizeDecimals)} {market.base}
            </span>
            <span className="num">${formatUsd(point.usd, { dp: 0 })}</span>
            {distance !== null && <span className="num depth-far">{formatPpm(distance, 2)} from mid</span>}
          </>
        ) : (
          <span className="depth-hint">{stale ? (book.ready ? 'Reconnecting: the book may be behind.' : 'Reading the book…') : `Cumulative size in the ${model.bids.length + model.asks.length} levels the exchange sent. Point at it, or use the arrow keys.`}</span>
        )}
      </p>
    </section>
  );
}
