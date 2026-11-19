/**
 * Order book and trade tape. Levels are aggregated to a multiple of the tick for reading only (bids down, asks up,
 * so a shown level is never better than the real one); the depth bars are geometry. A click on a price hands it to
 * the order form as a limit price; nothing is ever submitted from here.
 */
import { memo, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { feed } from '../state/session';
import { persisted, useStore } from '../lib/store';
import { formatClock, formatPpm, formatPx, formatSize, formatUsd, DASH } from '../lib/format';
import { mulDiv, toDecimalString, toNumber, PX_DECIMALS, SZ_DECIMALS } from '../lib/fixed';
import { notional, type Level } from '../lib/calc';
import type { Market } from '../lib/types';
import { pricePick } from './hooks';

/** Levels per side where the panel's height is not constrained (phone, stacked layouts). */
const ROWS_DEFAULT = 11;
const ROWS_MAX = 40;

export function aggregate(levels: Level[], group: bigint, bids: boolean): Level[] {
  if (group <= 0n) return levels;
  const out: Level[] = [];
  for (const l of levels) {
    const r = l.px % group;
    const px = r === 0n ? l.px : bids ? l.px - r : l.px - r + group;
    const last = out[out.length - 1];
    if (last && last.px === px) last.size += l.size;
    else out.push({ px, size: l.size });
  }
  return out;
}

/**
 * How many WHOLE rows fit on each side of the spread in a panel body of `height` px: the spread row is taken out
 * first and the rest is split evenly; never a cut row. Pure (tested).
 */
export function rowsThatFit(height: number, rowHeight: number, spreadHeight: number): number {
  if (!(height > 0) || !(rowHeight > 0)) return ROWS_DEFAULT;
  return Math.max(1, Math.min(ROWS_MAX, Math.floor((height - spreadHeight) / (2 * rowHeight))));
}

/** Which levels changed size since the previous book, and in which direction (for the flash). Pure (tested). */
export function changedLevels(previous: ReadonlyMap<string, bigint>, levels: readonly Level[]): Map<string, 'up' | 'down'> {
  const out = new Map<string, 'up' | 'down'>();
  for (const l of levels) {
    const before = previous.get(String(l.px));
    if (before === undefined) out.set(String(l.px), 'up');
    else if (before !== l.size) out.set(String(l.px), l.size > before ? 'up' : 'down');
  }
  return out;
}

type Unit = 'base' | 'usd';
type Bars = 'total' | 'size';
const bookUnit = persisted<Unit>('postern.perps.book.unit.v1', 'base', (v): v is Unit => v === 'base' || v === 'usd');
const bookBars = persisted<Bars>('postern.perps.book.bars.v1', 'total', (v): v is Bars => v === 'total' || v === 'size');

export function OrderBook({ market }: { market: Market }) {
  const book = useStore(feed.book);
  const connection = useStore(feed.connection);
  const unit = useStore(bookUnit);
  const bars = useStore(bookBars);
  const [mult, setMult] = useState(1n);
  const [rows, setRows] = useState(ROWS_DEFAULT);
  const bodyRef = useRef<HTMLDivElement>(null);
  // The number of rows follows the panel's height where the layout constrains it (the fitted desktop screen): whole
  // rows only. Where the body is as tall as its content (phone), the default count stays.
  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body || typeof ResizeObserver === 'undefined') return;
    const measure = () => {
      const constrained = getComputedStyle(body).getPropertyValue('--book-fit').trim() === '1';
      if (!constrained) return setRows(ROWS_DEFAULT);
      const row = parseFloat(getComputedStyle(body).getPropertyValue('--row-h')) || 22;
      const spread = body.querySelector<HTMLElement>('.book-spread')?.offsetHeight ?? 40;
      setRows(rowsThatFit(body.clientHeight, row, spread));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(body);
    return () => observer.disconnect();
  }, []);
  const group = market.tick * mult;
  const { asks, bids, max, spread, mid } = useMemo(() => {
    const a = aggregate(book.asks, group, false).slice(0, rows);
    const b = aggregate(book.bids, group, true).slice(0, rows);
    let cum = 0n;
    const ac = a.map((l) => ({ ...l, cum: (cum += l.size) }));
    cum = 0n;
    const bc = b.map((l) => ({ ...l, cum: (cum += l.size) }));
    const top = ac.length && bc.length ? ac[0].px - bc[0].px : null;
    const m = ac.length && bc.length ? (ac[0].px + bc[0].px) / 2n : null;
    // The bars of both sides share one scale, so the two sides can be compared by eye.
    const of = (l: { size: bigint; cum: bigint }) => (bars === 'total' ? l.cum : l.size);
    let peak = 1n;
    for (const l of [...ac, ...bc]) if (of(l) > peak) peak = of(l);
    return { asks: [...ac].reverse(), bids: bc, max: peak, spread: top, mid: m };
  }, [book, group, rows, bars]);
  // Flash: levels whose size changed since the last book that was drawn. Never animates a number: only the row's ground.
  const previous = useRef<Map<string, bigint>>(new Map());
  const symbolRef = useRef(market.symbol);
  const flashes = useMemo(() => {
    const all = [...asks, ...bids];
    const fresh = symbolRef.current !== market.symbol || previous.current.size === 0;
    const changed = fresh ? new Map<string, 'up' | 'down'>() : changedLevels(previous.current, all);
    previous.current = new Map(all.map((l) => [String(l.px), l.size]));
    symbolRef.current = market.symbol;
    return changed;
  }, [asks, bids, market.symbol]);
  const stale = !book.ready || connection.state !== 'open';
  const pick = (px: bigint) => pricePick.set({ text: toDecimalString(px / 10n ** BigInt(PX_DECIMALS - market.priceDecimals), market.priceDecimals), at: Date.now() });
  const empty = book.ready && asks.length === 0 && bids.length === 0;
  return (
    <section className={`panel book ${stale ? 'is-stale' : ''}`} aria-label="Order book">
      <header className="panel-head book-head">
        <h2>Order book</h2>
        <div className="book-tools">
          <div className="segmented segmented-micro" role="group" aria-label="Depth bars show">
            {(['total', 'size'] as Bars[]).map((b) => (
              <button key={b} type="button" aria-pressed={bars === b} onClick={() => bookBars.set(b)} title={b === 'total' ? 'Bars: cumulative size from the spread' : 'Bars: size at each level'}>
                {b === 'total' ? 'Total' : 'Size'}
              </button>
            ))}
          </div>
          <div className="segmented segmented-micro" role="group" aria-label="Quantities in">
            {(['base', 'usd'] as Unit[]).map((u) => (
              <button key={u} type="button" aria-pressed={unit === u} onClick={() => bookUnit.set(u)}>
                {u === 'base' ? market.base : 'USD'}
              </button>
            ))}
          </div>
          <label className="group-select">
            <span className="sr-only">Price grouping</span>
            <select value={String(mult)} onChange={(e) => setMult(BigInt(e.target.value))}>
              {[1n, 10n, 100n].map((m) => (
                <option key={String(m)} value={String(m)}>
                  {formatPx(market.tick * m, market.priceDecimals)}
                </option>
              ))}
            </select>
          </label>
        </div>
      </header>
      <div className="book-cols" aria-hidden="true">
        <span>Price (USD)</span>
        <span>Size ({unit === 'base' ? market.base : 'USD'})</span>
        <span>Total</span>
      </div>
      <div className="book-body" ref={bodyRef}>
        <BookSide levels={asks} max={max} side="ask" market={market} onPick={pick} unit={unit} bars={bars} flashes={flashes} seq={book.seq} />
        <div className="book-spread">
          <span className="num book-mid">{mid !== null ? formatPx(mid, market.priceDecimals) : DASH}</span>
          <span className="book-spread-label">Spread</span>
          <span className="num">{spread !== null ? formatPx(spread, market.priceDecimals) : DASH}</span>
          <span className="num">{spread !== null && mid ? formatPpm(mulDiv(spread, 1_000_000n, mid, 'ceil'), 3) : ''}</span>
        </div>
        <BookSide levels={bids} max={max} side="bid" market={market} onPick={pick} unit={unit} bars={bars} flashes={flashes} seq={book.seq} />
        {!book.ready && (
          <div className="book-skeleton" aria-hidden="true">
            {Array.from({ length: 8 }, (_, i) => (
              <span key={i} className="skeleton" />
            ))}
          </div>
        )}
        {empty && <p className="book-empty">No resting orders in this market yet.</p>}
      </div>
      {stale && <p className="book-stale" role="status">{book.ready ? 'Reconnecting: the book may be behind.' : 'Reading the book…'}</p>}
    </section>
  );
}

type Row = Level & { cum: bigint };
const BookSide = memo(function BookSide({ levels, max, side, market, onPick, unit, bars, flashes, seq }: { levels: Row[]; max: bigint; side: 'ask' | 'bid'; market: Market; onPick: (px: bigint) => void; unit: Unit; bars: Bars; flashes: ReadonlyMap<string, 'up' | 'down'>; seq: number }) {
  // USD figures are notional at the level's own price (cumulative: the sum of each level at its price), floored.
  let usdCum = 0n;
  const ordered = side === 'ask' ? [...levels].reverse() : levels;
  const usd = new Map<string, [bigint, bigint]>();
  for (const l of ordered) {
    const n = notional(l.size, l.px);
    usdCum += n;
    usd.set(String(l.px), [n, usdCum]);
  }
  const maxN = toNumber(max, SZ_DECIMALS);
  return (
    <ol className={`book-side book-${side}`}>
      {levels.map((l) => {
        const key = String(l.px);
        const flash = flashes.get(key);
        const [n, nCum] = usd.get(key)!;
        const width = Math.min(100, (toNumber(bars === 'total' ? l.cum : l.size, SZ_DECIMALS) / Math.max(1e-12, maxN)) * 100);
        return (
          <li key={key}>
            <button
              type="button"
              className="book-row"
              // Two names for one animation restart it when the same level changes again in the next book.
              data-flash={flash ? `${flash}-${seq % 2 ? 'a' : 'b'}` : undefined}
              onClick={() => onPick(l.px)}
              aria-label={`${side === 'ask' ? 'Ask' : 'Bid'} ${formatPx(l.px, market.priceDecimals)}, size ${formatSize(l.size, market.sizeDecimals)}: use as limit price`}
            >
              <span className="book-depth" style={{ width: `${width}%` }} />
              <span className="num book-px">{formatPx(l.px, market.priceDecimals)}</span>
              <span className="num">{unit === 'base' ? formatSize(l.size, market.sizeDecimals) : formatUsd(n, { dp: 0 })}</span>
              <span className="num book-cum">{unit === 'base' ? formatSize(l.cum, market.sizeDecimals) : formatUsd(nCum, { dp: 0 })}</span>
            </button>
          </li>
        );
      })}
    </ol>
  );
});

export function TradeTape({ market }: { market: Market }) {
  const trades = useStore(feed.trades);
  return (
    <section className="panel tape" aria-label="Recent trades">
      <header className="panel-head">
        <h2>Trades</h2>
      </header>
      <div className="book-cols" aria-hidden="true">
        <span>Price</span>
        <span>Size ({market.base})</span>
        <span>Time (UTC)</span>
      </div>
      <ol className="tape-list">
        {trades.slice(0, 40).map((t) => (
          <li key={t.id} className={t.buy ? 'up' : 'down'}>
            <span className="num">
              {formatPx(t.px, market.priceDecimals)}
              {t.liquidation && <span className="badge badge-danger tape-liq" title="Liquidation">L</span>}
            </span>
            <span className="num">{formatSize(t.size, market.sizeDecimals)}</span>
            <span className="num tape-time">{formatClock(t.time)}</span>
          </li>
        ))}
        {trades.length === 0 && <li className="empty">No trades yet.</li>}
      </ol>
    </section>
  );
}
