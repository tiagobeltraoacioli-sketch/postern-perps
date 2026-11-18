/** Markets: every market the exchange publishes, as the exchange reports it. Click a row to trade it. */
import { useMemo, useState } from 'react';
import { Search } from 'lucide-react';
import { feed, go } from '../state/session';
import { useStore } from '../lib/store';
import { formatCompactUsd, formatPpm, formatPx, formatRate, DASH } from '../lib/format';
import { notional } from '../lib/calc';
import { AssetMark } from '../trade/AssetMark';
import { signingProblem } from '../lib/pins';
import type { Market, Ticker } from '../lib/types';

type SortKey = 'symbol' | 'change' | 'volume' | 'oi' | 'funding';
const STATUS_WORDS: Record<Market['status'], string> = { active: 'Trading', halted: 'Halted', closeOnly: 'Close only', settled: 'Settled' };

export function sortMarkets(markets: Market[], tickers: ReadonlyMap<string, Ticker>, key: SortKey, descending: boolean): Market[] {
  const value = (m: Market): bigint | null => {
    const t = tickers.get(m.symbol);
    if (!t) return null;
    if (key === 'change') return t.changePpm;
    if (key === 'volume') return t.volume24h;
    if (key === 'oi') return notional(t.openInterest, t.mark);
    return t.fundingRate;
  };
  const out = [...markets];
  if (key === 'symbol') out.sort((a, b) => a.marketId - b.marketId);
  else
    out.sort((a, b) => {
      const va = value(a);
      const vb = value(b);
      // A market without a ticker sorts last whatever the direction: an unknown is never ranked as zero.
      if (va === null || vb === null) return va === vb ? a.marketId - b.marketId : va === null ? 1 : -1;
      return va === vb ? a.marketId - b.marketId : (va < vb ? -1 : 1) * (descending ? -1 : 1);
    });
  return out;
}

export function Markets() {
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const error = useStore(feed.marketsError);
  const [query, setQuery] = useState('');
  const [sort, setSort] = useState<{ key: SortKey; descending: boolean }>({ key: 'symbol', descending: false });
  const rows = useMemo(() => {
    const q = query.trim().toUpperCase();
    return sortMarkets(q ? markets.filter((m) => m.symbol.includes(q) || m.base.includes(q)) : markets, tickers, sort.key, sort.descending);
  }, [markets, tickers, query, sort]);
  const totals = useMemo(() => {
    let volume = 0n;
    let oi = 0n;
    for (const m of markets) {
      const t = tickers.get(m.symbol);
      if (!t) continue;
      volume += t.volume24h;
      oi += notional(t.openInterest, t.mark);
    }
    return { volume, oi };
  }, [markets, tickers]);
  const head = (key: SortKey, label: string) => (
    <th className="r" aria-sort={sort.key === key ? (sort.descending ? 'descending' : 'ascending') : 'none'}>
      <button type="button" className="th-sort" onClick={() => setSort((s) => ({ key, descending: s.key === key ? !s.descending : true }))}>
        {label}
        <span aria-hidden="true">{sort.key === key ? (sort.descending ? ' ↓' : ' ↑') : ''}</span>
      </button>
    </th>
  );
  return (
    <div className="page page-wide">
      <header className="page-head page-head-row">
        <div>
          <p className="eyebrow">Markets</p>
          <h1 className="display">Perpetual markets</h1>
          <p className="lead">USD-quoted perpetuals, margined and settled in bUSDC. Every figure is the exchange's own, read live.</p>
        </div>
        <dl className="head-stats">
          <div>
            <dt>24 h volume</dt>
            <dd className="num">${formatCompactUsd(totals.volume)}</dd>
          </div>
          <div>
            <dt>Open interest</dt>
            <dd className="num">${formatCompactUsd(totals.oi)}</dd>
          </div>
          <div>
            <dt>Markets</dt>
            <dd className="num">{markets.length || DASH}</dd>
          </div>
        </dl>
      </header>
      {error && <p className="callout callout-danger">{error}</p>}
      <section className="panel" aria-label="Markets">
        <header className="panel-head">
          <label className="search">
            <Search size={16} aria-hidden="true" />
            <span className="sr-only">Search markets</span>
            <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search" spellCheck={false} />
          </label>
          <span className="fine">Funding is the hourly rate that would be charged at the next funding time. Positive: longs pay shorts.</span>
        </header>
        <div className="table-wrap">
          <table className="markets-table">
            <thead>
              <tr>
                <th aria-sort={sort.key === 'symbol' ? 'ascending' : 'none'}>
                  <button type="button" className="th-sort" onClick={() => setSort({ key: 'symbol', descending: false })}>
                    Market
                  </button>
                </th>
                <th className="r">Mark price</th>
                <th className="r">Oracle index</th>
                {head('change', '24 h change')}
                {head('volume', '24 h volume')}
                {head('oi', 'Open interest')}
                {head('funding', 'Funding (1 h)')}
                <th className="r">Max leverage</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((m) => {
                const t = tickers.get(m.symbol);
                const pin = signingProblem(m);
                return (
                  <tr key={m.symbol} className="row-link" onClick={() => go({ page: 'trade', symbol: m.symbol })}>
                    <td>
                      <button type="button" className="market-cell" onClick={(e) => (e.stopPropagation(), go({ page: 'trade', symbol: m.symbol }))} aria-label={`Trade ${m.symbol}`}>
                        <AssetMark asset={m.base} size={26} />
                        <strong>{m.symbol}</strong>
                        <span className="sub">Perp</span>
                      </button>
                    </td>
                    <td className="r num strong">{t ? formatPx(t.mark, m.priceDecimals) : DASH}</td>
                    <td className="r num">{t ? formatPx(t.index, m.priceDecimals) : DASH}</td>
                    <td className={`r num ${t ? (t.changePpm >= 0n ? 'up' : 'down') : ''}`}>{t ? formatPpm(t.changePpm, 2, 'always') : DASH}</td>
                    <td className="r num">{t ? `$${formatCompactUsd(t.volume24h)}` : DASH}</td>
                    <td className="r num">{t ? `$${formatCompactUsd(notional(t.openInterest, t.mark))}` : DASH}</td>
                    <td className={`r num ${t ? (t.fundingRate >= 0n ? 'up' : 'down') : ''}`}>{t ? formatRate(t.fundingRate) : DASH}</td>
                    <td className="r num">{m.maxLeverage}x</td>
                    <td>
                      <span className={`badge ${m.status === 'active' ? 'status-credited' : m.status === 'halted' ? 'badge-danger' : 'badge-warn'}`}>{STATUS_WORDS[m.status]}</span>
                      {pin && <span className="badge badge-danger" title={pin}>signing refused</span>}
                    </td>
                  </tr>
                );
              })}
              {rows.length === 0 && (
                <tr>
                  <td colSpan={9} className="empty">
                    {markets.length ? 'No market matches.' : error ? 'Markets could not be read.' : 'Reading markets…'}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}
