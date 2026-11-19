import { useEffect, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { feed, go, clockSkewMs } from '../state/session';
import { useStore } from '../lib/store';
import { formatCompactUsd, formatCountdown, formatPpm, formatPx, formatRate, formatSize, DASH } from '../lib/format';
import { notional } from '../lib/calc';
import { useNow } from './hooks';
import type { Market, Ticker } from '../lib/types';
import { AssetMark } from './AssetMark';

const STATUS_WORDS: Record<Market['status'], string> = { active: 'Open', halted: 'Halted', closeOnly: 'Close only', settled: 'Settled' };

export function MarketBar({ market, ticker }: { market: Market | undefined; ticker: Ticker | undefined }) {
  // Countdowns run on the exchange's clock (local clock plus the skew measured from /v1/status).
  const skew = useStore(clockSkewMs) ?? 0;
  const now = useNow(1000) + skew;
  const dp = market?.priceDecimals ?? 2;
  const change = ticker?.changePpm ?? null;
  return (
    <section className="market-bar" aria-label="Market">
      <MarketPicker current={market} />
      {/* Scrolls sideways when the bar is narrow: reachable by keyboard for that. */}
      <dl className="market-stats" tabIndex={0} aria-label="Market statistics">
        <div className="stat stat-lead">
          <dt>Mark</dt>
          <dd className="num">{ticker ? formatPx(ticker.mark, dp) : DASH}</dd>
        </div>
        <div className="stat stat-index">
          <dt>Oracle index</dt>
          <dd className="num">{ticker ? formatPx(ticker.index, dp) : DASH}</dd>
        </div>
        <div className="stat">
          <dt>24 h change</dt>
          <dd className={`num ${change === null ? '' : change >= 0n ? 'up' : 'down'}`}>{change === null ? DASH : formatPpm(change, 2, 'always')}</dd>
        </div>
        <div className="stat stat-wide">
          <dt>24 h high / low</dt>
          <dd className="num">{ticker ? `${formatPx(ticker.high24h, dp)} / ${formatPx(ticker.low24h, dp)}` : DASH}</dd>
        </div>
        <div className="stat stat-volume">
          <dt>24 h volume</dt>
          <dd className="num">{ticker ? `$${formatCompactUsd(ticker.volume24h)}` : DASH}</dd>
        </div>
        <div className="stat stat-oi">
          <dt>Open interest</dt>
          <dd className="num" title={ticker && market ? `${formatSize(ticker.openInterest, market.sizeDecimals)} ${market.base}` : undefined}>
            {ticker && market ? `$${formatCompactUsd(notional(ticker.openInterest, ticker.mark))}` : DASH}
          </dd>
        </div>
        <div className="stat">
          <dt>Funding (1 h)</dt>
          <dd className="num">
            {ticker ? (
              <>
                <span className={ticker.fundingRate >= 0n ? 'up' : 'down'}>{formatRate(ticker.fundingRate)}</span>
                <span className="stat-sub" title="Time to the next funding payment">
                  {' '}
                  in {formatCountdown(ticker.nextFundingTime - now)}
                </span>
              </>
            ) : (
              DASH
            )}
          </dd>
        </div>
        {market && market.status !== 'active' && (
          <div className="stat">
            <dt>Status</dt>
            <dd className="badge badge-warn">{STATUS_WORDS[market.status]}</dd>
          </div>
        )}
      </dl>
    </section>
  );
}

function MarketPicker({ current }: { current: Market | undefined }) {
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false);
    document.addEventListener('mousedown', away);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('mousedown', away);
      document.removeEventListener('keydown', esc);
    };
  }, [open]);
  return (
    <div className="market-picker" ref={ref}>
      <button type="button" className="market-picker-button" aria-haspopup="listbox" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        {current && <AssetMark asset={current.base} size={28} />}
        <span className="market-symbol">{current?.symbol ?? 'Markets'}</span>
        <span className="market-kind">Perp</span>
        {current && <span className="market-lev">{current.maxLeverage}x</span>}
        <ChevronDown size={16} aria-hidden="true" />
      </button>
      {open && (
        <ul className="market-list" role="listbox" aria-label="Markets">
          {markets.map((m) => {
            const t = tickers.get(m.symbol);
            return (
              <li key={m.symbol} role="option" aria-selected={m.symbol === current?.symbol}>
                <button
                  type="button"
                  onClick={() => {
                    setOpen(false);
                    go({ page: 'trade', symbol: m.symbol });
                  }}
                >
                  <AssetMark asset={m.base} size={22} />
                  <span className="market-symbol">{m.symbol}</span>
                  <span className="market-lev">{m.maxLeverage}x</span>
                  <span className="num">{t ? formatPx(t.mark, m.priceDecimals) : DASH}</span>
                  <span className={`num ${t && t.changePpm >= 0n ? 'up' : 'down'}`}>{t ? formatPpm(t.changePpm, 2, 'always') : DASH}</span>
                  {m.status !== 'active' && <span className="badge badge-warn">{STATUS_WORDS[m.status]}</span>}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
