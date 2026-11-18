/**
 * Order book and trade tape. Levels are aggregated to a multiple of the tick for reading only (bids down, asks up,
 * so a shown level is never better than the real one); the depth bars are geometry. A click on a price hands it to
 * the order form as a limit price; nothing is ever submitted from here.
 */
import { memo, useMemo, useState } from 'react';
import { feed } from '../state/session';
import { useStore } from '../lib/store';
import { formatClock, formatPpm, formatPx, formatSize, DASH } from '../lib/format';
import { mulDiv, toDecimalString, toNumber, PX_DECIMALS, SZ_DECIMALS } from '../lib/fixed';
import type { Level } from '../lib/calc';
import type { Market } from '../lib/types';
import { pricePick } from './hooks';

/** Levels rendered per side. On the fitted desktop layout the panel shows as many as its height allows (asks are
 *  anchored to the spread and cut from the far end); elsewhere all of them. */
const ROWS = 14;

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

export function OrderBook({ market }: { market: Market }) {
  const book = useStore(feed.book);
  const connection = useStore(feed.connection);
  const [mult, setMult] = useState(1n);
  const group = market.tick * mult;
  const { asks, bids, max, spread, mid } = useMemo(() => {
    const a = aggregate(book.asks, group, false).slice(0, ROWS);
    const b = aggregate(book.bids, group, true).slice(0, ROWS);
    let cum = 0n;
    const ac = a.map((l) => ({ ...l, cum: (cum += l.size) }));
    cum = 0n;
    const bc = b.map((l) => ({ ...l, cum: (cum += l.size) }));
    const top = ac.length && bc.length ? ac[0].px - bc[0].px : null;
    const m = ac.length && bc.length ? (ac[0].px + bc[0].px) / 2n : null;
    const askMax = ac.length ? ac[ac.length - 1].cum : 0n;
    const bidMax = bc.length ? bc[bc.length - 1].cum : 0n;
    return { asks: [...ac].reverse(), bids: bc, max: askMax > bidMax ? askMax : bidMax || 1n, spread: top, mid: m };
  }, [book, group]);
  const stale = !book.ready || connection.state !== 'open';
  const pick = (px: bigint) => pricePick.set({ text: toDecimalString(px / 10n ** BigInt(PX_DECIMALS - market.priceDecimals), market.priceDecimals), at: Date.now() });
  return (
    <section className={`panel book ${stale ? 'is-stale' : ''}`} aria-label="Order book">
      <header className="panel-head">
        <h2>Order book</h2>
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
      </header>
      <div className="book-cols" aria-hidden="true">
        <span>Price</span>
        <span>Size ({market.base})</span>
        <span>Total</span>
      </div>
      <BookSide levels={asks} max={max} side="ask" market={market} onPick={pick} />
      <div className="book-spread">
        <span className="num book-mid">{mid !== null ? formatPx(mid, market.priceDecimals) : DASH}</span>
        <span className="num">Spread {spread !== null ? formatPx(spread, market.priceDecimals) : DASH}</span>
        <span className="num">{spread !== null && mid ? formatPpm(mulDiv(spread, 1_000_000n, mid, 'ceil'), 3) : ''}</span>
      </div>
      <BookSide levels={bids} max={max} side="bid" market={market} onPick={pick} />
      {stale && <p className="book-stale">{book.ready ? 'Reconnecting: the book may be behind.' : 'Reading the book…'}</p>}
    </section>
  );
}

const BookSide = memo(function BookSide({ levels, max, side, market, onPick }: { levels: (Level & { cum: bigint })[]; max: bigint; side: 'ask' | 'bid'; market: Market; onPick: (px: bigint) => void }) {
  return (
    <ol className={`book-side book-${side}`}>
      {levels.map((l) => (
        <li key={String(l.px)}>
          <button type="button" className="book-row" onClick={() => onPick(l.px)} aria-label={`${side === 'ask' ? 'Ask' : 'Bid'} ${formatPx(l.px, market.priceDecimals)}, size ${formatSize(l.size, market.sizeDecimals)}: use as limit price`}>
            <span className="book-depth" style={{ width: `${Math.min(100, (toNumber(l.cum, SZ_DECIMALS) / Math.max(1e-12, toNumber(max, SZ_DECIMALS))) * 100)}%` }} />
            <span className="num book-px">{formatPx(l.px, market.priceDecimals)}</span>
            <span className="num">{formatSize(l.size, market.sizeDecimals)}</span>
            <span className="num book-cum">{formatSize(l.cum, market.sizeDecimals)}</span>
          </button>
        </li>
      ))}
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
